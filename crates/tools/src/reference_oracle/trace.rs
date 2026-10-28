use super::ffi::Oracle;
use serde::Serialize;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write as _};
use std::path::Path;

pub(super) struct Event {
    pub(super) gyro: [f64; 3],
    pub(super) acceleration: [f64; 3],
    pub(super) mag: [f64; 3],
    pub(super) temperature: f64,
    pub(super) mag_dt: f64,
    pub(super) thermal_on: bool,
}

pub(super) fn event(case: usize, index: usize, last_mag: &mut Option<usize>) -> Event {
    let (gyro, acceleration) = match case {
        0 => ([0.0, 0.0, 0.0], [0.000001, 0.0, -1.0]),
        1 => (
            if index < 1600 {
                [0.12, -0.21, 0.3]
            } else {
                [20.0, 5.0, -10.0]
            },
            if !index.is_multiple_of(11) {
                [0.0, 0.0, 1.0]
            } else {
                [0.0, 0.0, 0.0]
            },
        ),
        2 => ([1.0, -2.0, 3.0], [0.0, 0.0, 1.0]),
        _ => unreachable!("oracle case must be 0, 1 or 2"),
    };
    let mag = if case == 1 && matches!(index % 7, 0 | 2) {
        [0.1, 0.3, -0.2]
    } else {
        [0.0, 0.0, 0.0]
    };
    // The original computes these values as f64, and converts to f32 only at FFI.
    let temperature = -15.0 + (index % 90) as f64 * 0.01;
    let mag_dt = last_mag.map_or(0.02, |last| (index - last) as f64 * 0.01);
    if mag != [0.0; 3] {
        *last_mag = Some(index);
    }
    Event {
        gyro,
        acceleration,
        mag,
        temperature,
        mag_dt,
        thermal_on: case == 2,
    }
}

// JSON's shortest conversion uses round-to-even; normalize its notation to
// match the original Python CSV spelling.
pub(super) struct PythonFloat(f64);

pub(super) fn python_float(value: f64) -> PythonFloat {
    PythonFloat(value)
}

struct FloatBuffer {
    bytes: [u8; 64],
    len: usize,
}

impl io::Write for FloatBuffer {
    fn write(&mut self, value: &[u8]) -> io::Result<usize> {
        let end = self
            .len
            .checked_add(value.len())
            .ok_or(io::ErrorKind::WriteZero)?;
        let destination = self
            .bytes
            .get_mut(self.len..end)
            .ok_or(io::ErrorKind::WriteZero)?;
        destination.copy_from_slice(value);
        self.len = end;
        Ok(value.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl fmt::Display for PythonFloat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_nan() {
            return formatter.write_str("nan");
        }
        if self.0.is_infinite() {
            return formatter.write_str(if self.0.is_sign_negative() {
                "-inf"
            } else {
                "inf"
            });
        }
        let mut buffer = FloatBuffer {
            bytes: [0; 64],
            len: 0,
        };
        serde_json::to_writer(&mut buffer, &self.0).map_err(|_| fmt::Error)?;
        let text = std::str::from_utf8(&buffer.bytes[..buffer.len]).map_err(|_| fmt::Error)?;
        if let Some((mantissa, exponent)) = text.split_once('e') {
            let exponent: i32 = exponent.parse().map_err(|_| fmt::Error)?;
            write!(formatter, "{mantissa}e{exponent:+03}")
        } else {
            // serde_json uses fixed notation for decimal exponent -5; Python
            // uses scientific notation below exponent -4.
            let (sign, unsigned) = text.strip_prefix('-').map_or(("", text), |s| ("-", s));
            if let Some(digits) = unsigned.strip_prefix("0.0000").filter(|s| !s.is_empty()) {
                let (first, rest) = digits.split_at(1);
                if rest.is_empty() {
                    write!(formatter, "{sign}{first}e-05")
                } else {
                    write!(formatter, "{sign}{first}.{rest}e-05")
                }
            } else {
                formatter.write_str(text)
            }
        }
    }
}

pub(super) fn write(path: &Path, oracle: &mut Oracle) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let file =
        File::create(path).map_err(|error| format!("creating {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    write_rows(&mut writer, oracle)
        .and_then(|()| writer.flush())
        .map_err(|error| format!("writing {}: {error}", path.display()))
}

fn write_rows(writer: &mut impl io::Write, oracle: &mut Oracle) -> io::Result<()> {
    write!(writer, "case,index,gx,gy,gz,ax,ay,az,mx,my,mz,temperature")?;
    for channel in 0..22 {
        write!(writer, ",s{channel}")?;
    }
    writer.write_all(b"\r\n")?;
    for case in 0..3 {
        oracle.init();
        let mut last_mag = None;
        for index in 0..2400 {
            let event = event(case, index, &mut last_mag);
            let result = oracle.step(&event);
            write!(writer, "{case},{index}")?;
            for value in event
                .gyro
                .into_iter()
                .chain(event.acceleration)
                .chain(event.mag)
                .chain([event.temperature])
            {
                write!(writer, ",{}", python_float(value))?;
            }
            for value in result {
                write!(writer, ",{}", python_float(f64::from(value)))?;
            }
            writer.write_all(b"\r\n")?;
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub(super) struct Baseline {
    pub(super) logical_id: &'static str,
    pub(super) sha256: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Comparison {
    pub(super) baseline: Baseline,
    pub(super) rows: usize,
    pub(super) max_abs_s0_through_s21: [f64; 22],
    pub(super) max_pose_degrees_6d_9d: [f64; 2],
    pub(super) rest_magdist_mismatches: [usize; 2],
}

fn open_csv(path: &Path) -> Result<csv::Reader<File>, String> {
    csv::ReaderBuilder::new()
        .flexible(true)
        .from_path(path)
        .map_err(|error| format!("reading {}: {error}", path.display()))
}

fn values(record: &csv::StringRecord) -> Result<[f64; 22], String> {
    if record.len() != 34 {
        return Err(format!(
            "Comparison trace row has {} columns, expected 34",
            record.len()
        ));
    }
    let mut values = [0.0; 22];
    for (value, text) in values.iter_mut().zip(record.iter().skip(12)) {
        *value = text
            .parse()
            .map_err(|error| format!("Invalid comparison value {text:?}: {error}"))?;
    }
    Ok(values)
}

// Python 3.12+ uses the Neumaier compensated sum for float iterables.
fn compensated_sum(values: impl Iterator<Item = f64>) -> f64 {
    let mut high = 0.0_f64;
    let mut low = 0.0_f64;
    for value in values {
        let next = high + value;
        if high.abs() >= value.abs() {
            low += (high - next) + value;
        } else {
            low += (value - next) + high;
        }
        high = next;
    }
    if low != 0.0 && low.is_finite() {
        high + low
    } else {
        high
    }
}

fn compare_row(
    result: &mut Comparison,
    left: &csv::StringRecord,
    right: &csv::StringRecord,
) -> Result<(), String> {
    if !left.iter().take(12).eq(right.iter().take(12)) {
        return Err("Comparison input events differ".to_owned());
    }
    let left = values(left)?;
    let right = values(right)?;
    for (channel, (&a, &b)) in left.iter().zip(&right).enumerate() {
        if a.is_finite() && b.is_finite() {
            result.max_abs_s0_through_s21[channel] =
                result.max_abs_s0_through_s21[channel].max((a - b).abs());
        } else if !(a.is_nan() && b.is_nan()) {
            return Err("Nonfinite comparison mismatch".to_owned());
        }
    }
    for (pose, start) in [0, 4].into_iter().enumerate() {
        let q = &left[start..start + 4];
        let r = &right[start..start + 4];
        let dot = compensated_sum(q.iter().zip(r).map(|(x, y)| x * y)).abs();
        let norm = (compensated_sum(q.iter().map(|x| x * x))
            * compensated_sum(r.iter().map(|x| x * x)))
        .sqrt();
        if norm == 0.0 {
            return Err("Zero-length quaternion in comparison trace".to_owned());
        }
        let degrees = (2.0 * (dot / norm).min(1.0).acos()).to_degrees();
        result.max_pose_degrees_6d_9d[pose] = result.max_pose_degrees_6d_9d[pose].max(degrees);
    }
    for (slot, channel) in [12, 13].into_iter().enumerate() {
        result.rest_magdist_mismatches[slot] += usize::from(left[channel] != right[channel]);
    }
    result.rows += 1;
    Ok(())
}

pub(super) fn compare(baseline_path: &Path, measured_path: &Path) -> Result<Comparison, String> {
    let mut baseline_reader = open_csv(baseline_path)?;
    let mut measured_reader = open_csv(measured_path)?;
    let mut baseline_rows = baseline_reader.records();
    let mut measured_rows = measured_reader.records();
    let mut result = Comparison {
        baseline: Baseline {
            logical_id: "comparison-baseline",
            sha256: super::file_sha256(baseline_path)?,
        },
        rows: 0,
        max_abs_s0_through_s21: [0.0; 22],
        max_pose_degrees_6d_9d: [0.0; 2],
        rest_magdist_mismatches: [0; 2],
    };
    // The original checks total length before reporting a per-row mismatch.
    let mut row_error = None;
    loop {
        let left = baseline_rows
            .next()
            .transpose()
            .map_err(|error| format!("reading baseline: {error}"))?;
        let right = measured_rows
            .next()
            .transpose()
            .map_err(|error| format!("reading measured trace: {error}"))?;
        let (left, right) = match (left, right) {
            (None, None) => break,
            (Some(left), Some(right)) => (left, right),
            _ => return Err("Comparison trace length differs".to_owned()),
        };
        if row_error.is_none() {
            row_error = compare_row(&mut result, &left, &right).err();
        }
    }
    if let Some(error) = row_error {
        return Err(error);
    }
    Ok(result)
}
