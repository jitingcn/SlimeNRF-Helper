use super::{
    Application, CalibrationTask, ConsoleValues, Engine, Fact, ObservationEnd, Phase, RunMode,
    RunRequest, Snapshot, Target,
};
use crate::domain::{DeviceIdentity, DeviceRole, normalize_address};
use crate::sessions::ReservedConsole;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const LINE_LIMIT: usize = 2_048;
const QUERY_BYTES: usize = 65_536;
const QUERY_LINES: usize = 1_024;
const QUERY_MS: u64 = 5_000;
const DRAIN_MS: u64 = 250;

pub(crate) trait DirectIo {
    fn now_ms(&self) -> u64;
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, String>;
    fn write_command(&mut self, bytes: &[u8], stop: &AtomicBool) -> Result<(), String>;
}

pub(crate) struct NativeDirectIo {
    console: ReservedConsole,
    started: Instant,
}

impl NativeDirectIo {
    pub(crate) fn new(console: ReservedConsole) -> Self {
        Self {
            console,
            started: Instant::now(),
        }
    }
}

impl DirectIo for NativeDirectIo {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }

    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, String> {
        self.console.read(bytes)
    }

    fn write_command(&mut self, bytes: &[u8], stop: &AtomicBool) -> Result<(), String> {
        self.console.write_command(bytes, stop)
    }
}

#[derive(Default, PartialEq, Eq)]
enum Escape {
    #[default]
    None,
    Start,
    Csi,
    Osc,
    OscEnd,
}

#[derive(Default)]
struct Lines {
    bytes: Vec<u8>,
    raw_length: usize,
    escape: Escape,
}

impl Lines {
    fn clean(&self) -> bool {
        self.raw_length == 0 && self.escape == Escape::None
    }

    fn byte(&mut self, byte: u8) -> Result<Option<String>, String> {
        if byte == b'\n' {
            if self.escape != Escape::None {
                return Err("Unterminated console escape sequence".into());
            }
            let line = String::from_utf8(std::mem::take(&mut self.bytes))
                .map_err(|_| "Malformed console UTF-8".to_owned())?;
            self.raw_length = 0;
            return Ok(Some(line));
        }
        self.raw_length += 1;
        if self.raw_length > LINE_LIMIT {
            return Err("Console line exceeds the 2 KiB host limit".into());
        }
        match self.escape {
            Escape::None => match byte {
                0x1b => self.escape = Escape::Start,
                b'\r' => {}
                0..=8 | 11..=12 | 14..=31 | 127 => {
                    return Err("Unexpected console control byte".into());
                }
                _ => self.bytes.push(byte),
            },
            Escape::Start => {
                self.escape = match byte {
                    b'[' => Escape::Csi,
                    b']' => Escape::Osc,
                    _ => return Err("Unsupported console escape sequence".into()),
                };
            }
            Escape::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    self.escape = Escape::None;
                } else if !(0x20..=0x3f).contains(&byte) {
                    return Err("Malformed console CSI sequence".into());
                }
            }
            Escape::Osc => match byte {
                7 => self.escape = Escape::None,
                0x1b => self.escape = Escape::OscEnd,
                _ => {}
            },
            Escape::OscEnd => {
                if byte != b'\\' {
                    return Err("Malformed console OSC sequence".into());
                }
                self.escape = Escape::None;
            }
        }
        Ok(None)
    }
}

fn log_payload(line: &str) -> Option<(&str, &str)> {
    let mut line = line.trim();
    if line.starts_with('[') {
        let (_, remaining) = line.split_once(']')?;
        line = remaining.trim_start();
    }
    if line.starts_with('<') {
        let (level, remaining) = line.split_once('>')?;
        if !matches!(level, "<inf" | "<wrn" | "<err" | "<dbg") {
            return None;
        }
        line = remaining.trim_start();
    }
    let (module, payload) = line.split_once(':')?;
    let module = module.trim();
    if module.is_empty()
        || !module
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return None;
    }
    Some((module, payload.trim()))
}

fn asynchronous(line: &str) -> bool {
    line.trim_start().starts_with(['[', '<'])
        || log_payload(line)
            .is_some_and(|(module, _)| matches!(module, "cal_imu" | "cal_sens" | "imu_calibration"))
}

fn decimal(value: &str, precision: usize) -> Option<f64> {
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let (integer, fraction) = unsigned.split_once('.')?;
    if integer.is_empty()
        || !integer.bytes().all(|c| c.is_ascii_digit())
        || fraction.len() != precision
        || !fraction.bytes().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    value.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn vector(value: &str) -> Option<[f64; 3]> {
    let mut words = value.split_whitespace();
    let result = [
        decimal(words.next()?, 5)?,
        decimal(words.next()?, 5)?,
        decimal(words.next()?, 5)?,
    ];
    words.next().is_none().then_some(result)
}

const INFO_FIELDS: [&str; 15] = [
    "Board",
    "SOC",
    "Target",
    "IMU",
    "Magnetometer",
    "ODR",
    "Gyroscope bias",
    "Fusion",
    "Tracker ID",
    "Device address",
    "Receiver address",
    "RF Channel",
    "Battery",
    "Remaining runtime",
    "Fully charged runtime",
];

#[derive(Default)]
struct Info {
    identity: DeviceIdentity,
    provenance: bool,
    next: usize,
    fields: [Option<String>; 15],
    bias: Option<[f64; 3]>,
    scales: Option<[f64; 3]>,
    scales_seen: bool,
}

impl Info {
    fn complete(&self) -> bool {
        self.next == 15
    }

    fn line(&mut self, line: &str) -> Result<(), String> {
        let line = line.trim();
        if line.is_empty() || asynchronous(line) {
            return Ok(());
        }
        if line == "=== Available Commands ===" {
            return Err("Overlapping console query responses".into());
        }
        self.identity.ingest_line(line);
        if self.identity.role == DeviceRole::Receiver {
            return Err("Receiver identity appeared in the tracker response".into());
        }
        if let Some(value) = line.strip_prefix("Repo: ") {
            if self.next != 0
                || self.provenance
                || !value
                    .split_once(" | Branch: ")
                    .is_some_and(|(repo, branch)| !repo.is_empty() && !branch.is_empty())
            {
                return Err("Conflicting console provenance".into());
            }
            self.provenance = true;
            return Ok(());
        }
        if let Some(value) = line.strip_prefix("Gyroscope sensitivity scale: ") {
            if self.next != 7 || self.scales_seen {
                return Err("Unassociated or repeated sensitivity field".into());
            }
            self.scales = Some(vector(value).ok_or("Malformed sensitivity scales")?);
            self.scales_seen = true;
            return Ok(());
        }
        if line == "Gyroscope sensitivity: Retained data unavailable." {
            if self.next != 7 || self.scales_seen {
                return Err("Conflicting sensitivity fields".into());
            }
            self.scales_seen = true;
            return Ok(());
        }
        let Some((key, value)) = line.split_once(':') else {
            if self.next == 0 {
                self.identity.ingest_line(line);
            }
            return Ok(());
        };
        let Some(index) = INFO_FIELDS.iter().position(|field| *field == key) else {
            return Ok(());
        };
        if index != self.next || value.trim().is_empty() || value.contains(['[', '<']) {
            return Err(format!(
                "Incomplete, repeated or unordered info field {key}"
            ));
        }
        let value = value.trim();
        if index == 0 && (!self.provenance || self.identity.role != DeviceRole::Tracker) {
            return Err("Console did not positively identify a supported tracker profile".into());
        }
        match index {
            0..=2 if value.contains(char::is_whitespace) => {
                return Err("Malformed board, SOC or target identity".into());
            }
            4 if !value.ends_with(" (enabled)") && !value.ends_with(" (disabled)") => {
                return Err("Malformed magnetometer identity".into());
            }
            5 if !odr(value) => return Err("Malformed sensor rate summary".into()),
            6 => self.bias = Some(vector(value).ok_or("Malformed applied gyro-bias snapshot")?),
            8 if value != "None" && !value.parse::<u8>().is_ok_and(|slot| slot < 16) => {
                return Err("Malformed Tracker ID".into());
            }
            9 if normalize_address(value).is_none() => {
                return Err("Malformed Device address".into());
            }
            10 if value != "None" && normalize_address(value).is_none() => {
                return Err("Malformed Receiver address".into());
            }
            11 => {
                let Some((channel, kind)) = value.split_once(' ') else {
                    return Err("Malformed RF channel context".into());
                };
                if !channel.parse::<u8>().is_ok_and(|channel| channel <= 100)
                    || !matches!(kind, "(default)" | "(custom)")
                {
                    return Err("Malformed RF channel context".into());
                }
            }
            13 | 14 if !runtime(value) => return Err("Malformed battery runtime".into()),
            _ => {}
        }
        self.fields[index] = Some(value.to_owned());
        self.next += 1;
        Ok(())
    }

    fn address(&self) -> String {
        normalize_address(self.fields[9].as_deref().expect("complete info address"))
            .expect("validated info address")
    }

    fn values(&self, start: u64, end: u64) -> ConsoleValues {
        ConsoleValues {
            bias: self.bias,
            scales: self.scales,
            query_start_ms: start,
            query_end_ms: end,
            native_open: 0,
        }
    }
}

fn odr(value: &str) -> bool {
    let Some(value) = value.strip_prefix("gyro ") else {
        return false;
    };
    let Some((gyro, value)) = value.split_once("Hz / accel ") else {
        return false;
    };
    let Some((accel, value)) = value.split_once("Hz / mag ") else {
        return false;
    };
    let Some((mag, fusion)) = value.split_once(" | fusion ") else {
        return false;
    };
    let valid_rate = |rate: &str| decimal(rate, 1).is_some_and(|rate| rate >= 0.0);
    valid_rate(gyro)
        && valid_rate(accel)
        && (mag == "n/a" || mag.strip_suffix("Hz").is_some_and(valid_rate))
        && fusion.strip_suffix("Hz").is_some_and(valid_rate)
}

fn runtime(value: &str) -> bool {
    if value == "Not available" {
        return true;
    }
    let Some((hours, minutes)) = value.split_once("h ") else {
        return false;
    };
    hours.parse::<u32>().is_ok()
        && minutes
            .strip_suffix("min")
            .is_some_and(|minutes| minutes.parse::<u8>().is_ok_and(|v| v < 60))
}

const HELP_SECTIONS: [&str; 7] = [
    "Device Information:",
    "Sensor Management:",
    "Connection:",
    "System:",
    "Other:",
    "Debug Commands:",
    "Button Functions (current build):",
];

#[derive(Default)]
struct Help {
    header: bool,
    section: usize,
    rows: usize,
    button_end: bool,
    heater: u8,
    complete: bool,
    stationary: bool,
    sensitivity: bool,
}

impl Help {
    fn line(&mut self, line: &str) -> Result<(), String> {
        if asynchronous(line) {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed == "=== Available Commands ===" {
            if self.header {
                return Err("Repeated help response".into());
            }
            self.header = true;
            return Ok(());
        }
        if !self.header {
            if trimmed.is_empty() || trimmed == "help" {
                return Ok(());
            }
            return Err("Unsupported help response header".into());
        }
        if trimmed.is_empty() {
            if self.section == 7 && self.button_end && self.heater != 1 {
                self.complete = true;
            }
            return Ok(());
        }
        if self.complete {
            return Err("Unexpected help response tail".into());
        }
        if self.section < 7 && trimmed == HELP_SECTIONS[self.section] {
            if self.section != 0 && self.rows == 0 {
                return Err("Empty help command section".into());
            }
            self.section += 1;
            self.rows = 0;
            return Ok(());
        }
        if HELP_SECTIONS.contains(&trimmed) || self.section == 0 || !line.starts_with("  ") {
            return Err("Unsupported or unordered help section".into());
        }
        self.rows += 1;
        if self.section == 2 {
            let words = trimmed.split_whitespace().collect::<Vec<_>>();
            if words == ["calibrate", "Calibrate", "sensor", "ZRO"] {
                self.stationary = true;
            }
            if let Some(rest) = trimmed.strip_prefix("sens auto <x|y|z> [rev]") {
                let rest = rest.trim();
                let prefix = "Auto-calibrate gyro sensitivity by spinning (default ";
                self.sensitivity = rest
                    .strip_prefix(prefix)
                    .and_then(|value| value.strip_suffix(" rev)"))
                    .is_some_and(|v| v.parse::<u16>().is_ok_and(|v| (1..=100).contains(&v)));
                if !self.sensitivity {
                    return Err("Unsupported sens auto advertisement".into());
                }
            }
        }
        if self.section == 7 {
            let compact = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
            if compact == "During OTA: Button actions are blocked"
                || compact == "No sw0 button is defined for this board"
            {
                if self.button_end {
                    return Err("Repeated button help ending".into());
                }
                self.button_end = true;
            } else if let Some(value) =
                compact.strip_prefix("Heater button hold (3s): Start heated T-Cal at ")
            {
                if !self.button_end
                    || self.heater != 0
                    || !value
                        .strip_suffix(" C (release first)")
                        .is_some_and(|v| v.parse::<i32>().is_ok())
                {
                    return Err("Malformed heater button help".into());
                }
                self.heater = 1;
            } else if compact == "Ignored during OTA or an active heated session"
                && self.heater == 1
            {
                self.heater = 2;
            } else if self.button_end || !button_row(&compact) {
                return Err("Unsupported button help ending".into());
            }
        }
        Ok(())
    }
}

fn button_row(line: &str) -> bool {
    matches!(
        line,
        "Short press (1x): Reboot (blocked in test mode)"
            | "Quick press (2x): Calibrate sensor ZRO"
            | "Quick press (3x): Reset active pairing"
            | "Quick press (4x/5x): Enter DFU bootloader"
            | "Quick press (6x/7x): Enter DFU bootloader"
            | "Quick press (8x/9x): Enter DFU bootloader"
            | "Quick press (8x/9x): Enter OTA DFU (BLE) if flashed SD (softdevice) firmware"
            | "Hold (~1s): Power off; keep holding ~5s to cancel"
            | "Hold (~1s): Reboot; keep holding ~5s to cancel"
            | "Hold (~1s): Power off; keep holding ~5s to reset pairing"
            | "Hold (~1s): Reboot; keep holding ~5s to reset pairing"
            | "Hold while USB connects: Enter DFU bootloader"
    )
}

enum Query<'a> {
    Help(&'a mut Help),
    Info(&'a mut Info),
}

impl Query<'_> {
    fn line(&mut self, line: &str) -> Result<(), String> {
        if line == "Command unavailable in this firmware"
            || line == "Command unavailable in this firmware."
        {
            return Err("Command unavailable in this firmware".into());
        }
        match self {
            Self::Help(help) => help.line(line),
            Self::Info(info) => info.line(line),
        }
    }

    fn complete(&self) -> bool {
        match self {
            Self::Help(help) => help.complete,
            Self::Info(info) => info.complete(),
        }
    }
}

struct Operation {
    task: CalibrationTask,
    armed: bool,
    recovering: bool,
    identity_verified: bool,
    pending: Vec<(u64, String)>,
    pending_bytes: usize,
    started: bool,
    stationary_start: bool,
    applied: Option<String>,
}

impl Operation {
    fn line(&mut self, line: &str, engine: &mut Engine, now: u64) -> Result<(), QueryError> {
        let line = line.trim();
        if self.armed && !self.recovering && line == "Command unavailable in this firmware" {
            engine.fact(Fact::DispatchRejected, now);
            engine.fact(Fact::Application(Application::Rejected), now);
            return Ok(());
        }
        let log = log_payload(line);
        let payload = log.map_or(line, |(_, payload)| payload);
        let relevant = payload.starts_with("Gyro sensitivity auto-calibration ")
            || request_rejected(line)
            || log.is_some_and(|(module, _)| {
                matches!(module, "cal_imu" | "cal_sens" | "imu_calibration")
            });
        if !relevant {
            return Ok(());
        }
        if !self.armed {
            return Err("Calibration activity preceded the owned operation write".into());
        }
        if self.recovering && !self.identity_verified {
            self.pending_bytes += line.len();
            if self.pending_bytes > QUERY_BYTES || self.pending.len() == QUERY_LINES {
                return Err("Recovery console evidence exceeded its preparation bound".into());
            }
            self.pending.push((now, line.to_owned()));
            return Ok(());
        }
        match self.task {
            CalibrationTask::Sensitivity { axis, revolutions } => {
                if log.is_some_and(|(module, payload)| {
                    module == "cal_imu"
                        && payload
                            == "Calibrating main accelerometer and gyroscope zero rate offset"
                }) {
                    return Err(QueryError::Conflict(
                        "Another calibration kind appeared during sensitivity calibration".into(),
                    ));
                }
                if let Some(rest) =
                    line.strip_prefix("Gyro sensitivity auto-calibration started on ")
                {
                    let expected = format!(
                        "{} axis ({} rev).",
                        axis.label().to_ascii_uppercase(),
                        revolutions.get()
                    );
                    if self.recovering || rest != expected || self.started {
                        return Err(QueryError::Conflict(
                            "Conflicting sensitivity admission or duplicate operation start".into(),
                        ));
                    }
                    self.started = true;
                    engine.fact(Fact::ConsoleAccepted, now);
                    engine.fact(Fact::Phase(Phase::WaitingStill), now);
                } else if let Some(rest) =
                    line.strip_prefix("Gyro sensitivity auto-calibration applied: axis ")
                {
                    let observed_axis =
                        sens_applied(rest).ok_or("Malformed sensitivity applied result")?;
                    if observed_axis != axis.label().to_ascii_uppercase() {
                        return Err(QueryError::Conflict(
                            "Sensitivity result reported another axis".into(),
                        ));
                    }
                    if self
                        .applied
                        .as_deref()
                        .is_some_and(|previous| previous != line)
                    {
                        return Err(QueryError::Conflict(
                            "Conflicting sensitivity applied payloads".into(),
                        ));
                    }
                    self.applied = Some(line.to_owned());
                    engine.fact(Fact::Application(Application::Applied), now);
                } else if sens_failed(line) {
                    engine.fact(Fact::Application(Application::Failed), now);
                } else if request_rejected(line) {
                    if self.recovering {
                        return Err(QueryError::Conflict(
                            "A new request admission appeared during recovery".into(),
                        ));
                    }
                    engine.fact(Fact::DispatchRejected, now);
                    engine.fact(Fact::Application(Application::Rejected), now);
                } else if sens_rejected(line) {
                    engine.fact(Fact::Application(Application::Rejected), now);
                } else if let Some(("cal_sens", payload)) = log {
                    if let Some(value) = payload.strip_prefix("Sensitivity calibration: axis ") {
                        if let Some((observed_axis, scale)) = value.split_once(" scale set to ") {
                            if observed_axis != axis.label().to_ascii_uppercase() {
                                return Err(QueryError::Conflict(
                                    "Sensitivity scale log reported another axis".into(),
                                ));
                            }
                            decimal(scale, 5).ok_or("Malformed sensitivity scale log")?;
                        } else {
                            if self.recovering {
                                return Err(QueryError::Conflict(
                                    "A new sensitivity measurement appeared during recovery".into(),
                                ));
                            }
                            let expected = format!(
                                "{}, {} rev ({:.1} deg expected)",
                                axis.label().to_ascii_uppercase(),
                                revolutions.get(),
                                f64::from(revolutions.get()) * 360.0
                            );
                            if value != expected {
                                return Err(QueryError::Conflict(
                                    "Sensitivity measurement parameters did not match the request"
                                        .into(),
                                ));
                            }
                        }
                    }
                    let phase = match payload {
                        "Sensitivity calibration: hold still" => Some(Phase::WaitingStill),
                        "Sensitivity calibration: recording" => Some(Phase::Recording),
                        _ if payload
                            .strip_prefix("Sensitivity calibration: computed scale ")
                            .is_some_and(|v| decimal(v, 5).is_some()) =>
                        {
                            Some(Phase::Validating)
                        }
                        _ if payload
                            == format!(
                                "Sensitivity calibration: spin the tracker about the {} axis now",
                                axis.label().to_ascii_uppercase()
                            ) =>
                        {
                            Some(Phase::WaitingRotation)
                        }
                        _ if payload.starts_with(
                            "Sensitivity calibration: spin the tracker about the ",
                        ) =>
                        {
                            return Err(QueryError::Conflict(
                                "Sensitivity rotation cue reported another axis".into(),
                            ));
                        }
                        _ => None,
                    };
                    if let Some(phase) = phase {
                        engine.fact(Fact::Phase(phase), now);
                    }
                    if negative_code(
                        payload,
                        "Sensitivity calibration applied in RAM; persistence failed: ",
                    ) {
                        engine.fact(Fact::Application(Application::Applied), now);
                        engine.fact(Fact::StorageError, now);
                    }
                } else if line.starts_with("Gyro sensitivity auto-calibration ") {
                    return Err("Unsupported sensitivity result grammar".into());
                }
            }
            CalibrationTask::Stationary => {
                if log.is_some_and(|(module, _)| module == "cal_sens") {
                    return Err(QueryError::Conflict(
                        "Sensitivity activity appeared during stationary calibration".into(),
                    ));
                }
                if let Some(("cal_imu", payload)) = log {
                    match payload {
                        "Calibrating main accelerometer and gyroscope zero rate offset" => {
                            if self.recovering || self.stationary_start {
                                return Err(QueryError::Conflict(
                                    "Duplicate stationary operation start".into(),
                                ));
                            }
                            self.stationary_start = true;
                            engine.fact(Fact::Phase(Phase::WaitingStill), now);
                        }
                        "Rest the device on a stable surface" => {
                            engine.fact(Fact::Phase(Phase::WaitingStill), now)
                        }
                        "Running BMI270 component retrimming" => {
                            engine.fact(Fact::Phase(Phase::Retrimming), now)
                        }
                        "Reading data" => engine.fact(Fact::Phase(Phase::Collecting), now),
                        "Calibration queued for next sensor frame" => {
                            engine.fact(Fact::Application(Application::CandidateQueued), now)
                        }
                        "Previous calibration unchanged"
                        | "IMU specific calibration was not completed properly" => {
                            engine.fact(Fact::Application(Application::Failed), now);
                        }
                        _ if negative_code(payload, "Cannot safely suspend for IMU retrim: ") => {
                            engine.fact(Fact::Application(Application::Failed), now);
                        }
                        _ if payload
                            .strip_prefix("Calibration candidate rejected: ")
                            .and_then(|v| v.strip_suffix("; previous calibration unchanged"))
                            .is_some_and(|v| v.parse::<i32>().is_ok()) =>
                        {
                            engine.fact(Fact::Application(Application::Rejected), now);
                        }
                        _ => {}
                    }
                } else if let Some(("imu_calibration", payload)) = log {
                    if negative_code(payload, "Calibration applied in RAM; persistence failed: ") {
                        engine.fact(Fact::Application(Application::Applied), now);
                        engine.fact(Fact::StorageError, now);
                    }
                } else if payload.starts_with("Gyro sensitivity auto-calibration ") {
                    return Err(QueryError::Conflict(
                        "Another calibration kind appeared during stationary calibration".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn negative_code(line: &str, prefix: &str) -> bool {
    line.strip_prefix(prefix)
        .is_some_and(|v| v.parse::<i32>().is_ok_and(|v| v < 0))
}

fn request_rejected(line: &str) -> bool {
    line.strip_prefix("Error: Calibration request rejected: ")
        .and_then(|v| v.strip_suffix('.'))
        .is_some_and(|v| v.parse::<i32>().is_ok())
}

fn sens_failed(line: &str) -> bool {
    matches!(
        line,
        "Gyro sensitivity auto-calibration failed: invalid parameters."
            | "Gyro sensitivity auto-calibration failed: tracker was not still."
            | "Gyro sensitivity auto-calibration failed: gyro timeout while measuring bias."
            | "Gyro sensitivity auto-calibration failed: no bias samples."
            | "Gyro sensitivity auto-calibration failed: no spin detected."
            | "Gyro sensitivity auto-calibration failed: gyro timeout during spin."
            | "Gyro sensitivity auto-calibration failed: spin did not complete in time."
            | "Gyro sensitivity auto-calibration failed: measured angle too small."
            | "Gyro sensitivity auto-calibration failed: retained data unavailable."
    )
}

fn sens_rejected(line: &str) -> bool {
    if line == "Gyro sensitivity auto-calibration rejected: invalid scale. Nothing saved." {
        return true;
    }
    if let Some(value) = line
        .strip_prefix("Gyro sensitivity auto-calibration rejected: too much off-axis motion (")
        .and_then(|v| v.strip_suffix("). Nothing saved."))
    {
        return value
            .split_once(" > ")
            .is_some_and(|(a, b)| decimal(a, 2).is_some() && decimal(b, 2).is_some());
    }
    let Some(value) = line
        .strip_prefix("Gyro sensitivity auto-calibration rejected: measured ")
        .and_then(|v| v.strip_suffix(" deg. Nothing saved."))
    else {
        return false;
    };
    let Some((measured, value)) = value.split_once(" deg for ") else {
        return false;
    };
    let Some((expected, value)) = value.split_once(" deg, scale ") else {
        return false;
    };
    let Some((scale, value)) = value.split_once(" outside ") else {
        return false;
    };
    let Some((range, value)) = value.split_once(". Equivalent sens diff over ") else {
        return false;
    };
    let Some((low, high)) = range.split_once("..") else {
        return false;
    };
    let Some((revs, difference)) = value.split_once(" rev: ") else {
        return false;
    };
    decimal(measured, 2).is_some()
        && decimal(expected, 2).is_some()
        && decimal(scale, 5).is_some()
        && decimal(low, 2).is_some()
        && decimal(high, 2).is_some()
        && revs.parse::<u16>().is_ok_and(|v| v > 0)
        && decimal(difference, 3).is_some()
}

fn sens_applied(value: &str) -> Option<&str> {
    let (axis, value) = value.split_once(", scale ")?;
    if !matches!(axis, "X" | "Y" | "Z") {
        return None;
    }
    let (scale, value) = value.split_once(", equivalent sens diff over ")?;
    decimal(scale, 5)?;
    let (revolutions, value) = value.split_once(" rev: ")?;
    if revolutions.parse::<u16>().ok()? == 0 {
        return None;
    }
    let (difference, value) = value.split_once(" deg, off-axis ")?;
    decimal(difference, 3)?;
    let ratio = value
        .strip_suffix(". Axis alignment was loose; repeating may improve accuracy.")
        .or_else(|| value.strip_suffix('.'))?;
    decimal(ratio, 2)?;
    Some(axis)
}

#[derive(Debug)]
enum QueryError {
    Stopped,
    Deadline,
    Invalid(String),
    Conflict(String),
    Io(String),
}

impl From<&str> for QueryError {
    fn from(message: &str) -> Self {
        Self::Invalid(message.to_owned())
    }
}

struct Console {
    lines: Lines,
    operation: Operation,
}

impl Console {
    fn read_lines(
        &mut self,
        io: &mut dyn DirectIo,
        engine: &mut Engine,
        publish: &dyn Fn(&Snapshot),
    ) -> Result<(usize, Vec<String>), QueryError> {
        let mut bytes = [0; 2_048];
        let length = io.read(&mut bytes).map_err(QueryError::Io)?;
        let mut lines = Vec::new();
        for &byte in &bytes[..length] {
            if let Some(line) = self.lines.byte(byte).map_err(QueryError::Invalid)? {
                self.operation.line(&line, engine, io.now_ms())?;
                lines.push(line);
            }
        }
        publish(&engine.snapshot);
        Ok((length, lines))
    }

    fn drain(
        &mut self,
        io: &mut dyn DirectIo,
        engine: &mut Engine,
        stop: &AtomicBool,
        publish: &dyn Fn(&Snapshot),
    ) -> Result<(), QueryError> {
        let start = io.now_ms();
        let mut bytes = 0;
        loop {
            if stop.load(Ordering::Acquire) {
                return Err(QueryError::Stopped);
            }
            if self.operation.armed && engine.expired(io.now_ms()) {
                return Err(QueryError::Deadline);
            }
            let (length, lines) = self.read_lines(io, engine, publish)?;
            bytes += length;
            for line in lines {
                if !asynchronous(&line)
                    && (line.trim() == "=== Available Commands ===" || line.starts_with("Board:"))
                {
                    return Err(QueryError::Invalid(
                        "Unsolicited prior query response quarantined this console".into(),
                    ));
                }
            }
            if length == 0 {
                if !self.lines.clean() {
                    return Err(QueryError::Invalid(
                        "Incomplete prior console response quarantined this console".into(),
                    ));
                }
                return Ok(());
            }
            if bytes >= QUERY_BYTES || io.now_ms().saturating_sub(start) >= DRAIN_MS {
                return Err(QueryError::Invalid(
                    "Console input barrier remained saturated".into(),
                ));
            }
        }
    }

    fn query(
        &mut self,
        mut query: Query<'_>,
        io: &mut dyn DirectIo,
        engine: &mut Engine,
        stop: &AtomicBool,
        publish: &dyn Fn(&Snapshot),
    ) -> Result<(u64, u64), QueryError> {
        self.drain(io, engine, stop, publish)?;
        if stop.load(Ordering::Acquire) {
            return Err(QueryError::Stopped);
        }
        let start = io.now_ms();
        let command: &[u8] = match &query {
            Query::Help(_) => b"help\n",
            Query::Info(_) => b"info\n",
        };
        io.write_command(command, stop).map_err(QueryError::Io)?;
        let mut bytes = 0;
        let mut count = 0;
        loop {
            if stop.load(Ordering::Acquire) {
                return Err(QueryError::Stopped);
            }
            if self.operation.armed && engine.expired(io.now_ms()) {
                return Err(QueryError::Deadline);
            }
            if io.now_ms().saturating_sub(start) >= QUERY_MS {
                return Err(QueryError::Invalid("Incomplete console query exceeded the 5-second host limit; console quarantined".into()));
            }
            let (length, lines) = self.read_lines(io, engine, publish)?;
            bytes += length;
            count += lines.len();
            if bytes > QUERY_BYTES || count > QUERY_LINES {
                return Err(QueryError::Invalid(
                    "Console query exceeded its bounded response limit".into(),
                ));
            }
            for line in lines {
                if self.operation.armed
                    && (line.starts_with("Gyro sensitivity auto-calibration ")
                        || request_rejected(&line))
                {
                    continue;
                }
                query.line(&line).map_err(QueryError::Invalid)?;
            }
            if query.complete() {
                return Ok((start, io.now_ms()));
            }
        }
    }
}

fn finish_query_error(
    error: QueryError,
    io: &dyn DirectIo,
    engine: &mut Engine,
    publish: &dyn Fn(&Snapshot),
    readback: bool,
) -> Result<(), String> {
    if readback {
        engine.fact(Fact::ReadbackIncomplete, io.now_ms());
    }
    match error {
        QueryError::Io(error) => Err(error),
        QueryError::Deadline => {
            engine.finish(engine.deadline_end(io.now_ms()), io.now_ms());
            publish(&engine.snapshot);
            Ok(())
        }
        QueryError::Stopped => {
            engine.finish(ObservationEnd::Stopped, io.now_ms());
            publish(&engine.snapshot);
            Ok(())
        }
        QueryError::Conflict(error) => {
            engine.fact(Fact::Application(Application::Conflicting), io.now_ms());
            engine.fact(Fact::Uncertain(error), io.now_ms());
            engine.finish(ObservationEnd::Ambiguous, io.now_ms());
            publish(&engine.snapshot);
            Ok(())
        }
        QueryError::Invalid(error) => {
            engine.fact(Fact::Uncertain(error), io.now_ms());
            engine.finish(
                if readback {
                    ObservationEnd::Ambiguous
                } else {
                    ObservationEnd::Unsupported
                },
                io.now_ms(),
            );
            publish(&engine.snapshot);
            Ok(())
        }
    }
}

pub(crate) fn run(
    request: &RunRequest,
    io: &mut dyn DirectIo,
    engine: &mut Engine,
    stop: &AtomicBool,
    publish: &dyn Fn(&Snapshot),
) -> Result<(), String> {
    let mut console = Console {
        lines: Lines::default(),
        operation: Operation {
            task: request.task,
            armed: request.mode == RunMode::Recover,
            recovering: request.mode == RunMode::Recover,
            identity_verified: false,
            pending: Vec::new(),
            pending_bytes: 0,
            started: false,
            stationary_start: false,
            applied: None,
        },
    };
    let mut help = Help::default();
    if let Err(error) = console.query(Query::Help(&mut help), io, engine, stop, publish) {
        return finish_query_error(error, io, engine, publish, request.mode == RunMode::Recover);
    }
    let mut info = Info::default();
    let (start, end) = match console.query(Query::Info(&mut info), io, engine, stop, publish) {
        Ok(result) => result,
        Err(error) => {
            return finish_query_error(
                error,
                io,
                engine,
                publish,
                request.mode == RunMode::Recover,
            );
        }
    };
    let address = info.address();
    let Target::Direct {
        address: expected, ..
    } = &request.target
    else {
        return Err("Direct worker received a non-console target".into());
    };
    if expected
        .as_deref()
        .is_some_and(|expected| normalize_address(expected).as_deref() != Some(address.as_str()))
        || (request.mode != RunMode::Inspect && expected.is_none())
    {
        engine.fact(
            Fact::Uncertain("Fresh console identity did not match the reviewed tracker".into()),
            io.now_ms(),
        );
        engine.finish(ObservationEnd::Ambiguous, io.now_ms());
        publish(&engine.snapshot);
        return Ok(());
    }
    engine.fact(
        Fact::Identity {
            address,
            stationary: help.stationary,
            sensitivity: help.sensitivity,
        },
        io.now_ms(),
    );
    console.operation.identity_verified = true;
    for (now, line) in std::mem::take(&mut console.operation.pending) {
        if let Err(error) = console.operation.line(&line, engine, now) {
            return finish_query_error(error, io, engine, publish, true);
        }
    }
    if request.mode == RunMode::Recover {
        engine.fact(Fact::Readback(info.values(start, end)), io.now_ms());
    } else {
        engine.fact(Fact::Baseline(info.values(start, end)), io.now_ms());
    }
    publish(&engine.snapshot);
    if request.mode == RunMode::Inspect {
        engine.finish(ObservationEnd::Complete, io.now_ms());
        publish(&engine.snapshot);
        return Ok(());
    }
    if request.mode == RunMode::Execute {
        if !match request.task {
            CalibrationTask::Stationary => help.stationary,
            CalibrationTask::Sensitivity { .. } => help.sensitivity,
        } {
            engine.finish(ObservationEnd::Unsupported, io.now_ms());
            publish(&engine.snapshot);
            return Ok(());
        }
        if let Err(error) = console.drain(io, engine, stop, publish) {
            return finish_query_error(error, io, engine, publish, false);
        }
        if stop.load(Ordering::Acquire) {
            engine.finish(ObservationEnd::Stopped, io.now_ms());
            publish(&engine.snapshot);
            return Ok(());
        }
        let command = match request.task {
            CalibrationTask::Stationary => "calibrate\n".to_owned(),
            CalibrationTask::Sensitivity { axis, revolutions } => {
                format!("sens auto {} {}\n", axis.label(), revolutions.get())
            }
        };
        console.operation.armed = true;
        engine.fact(Fact::DispatchAttempt, io.now_ms());
        publish(&engine.snapshot);
        io.write_command(command.as_bytes(), stop)?;
        engine.fact(Fact::Written, io.now_ms());
        publish(&engine.snapshot);
    }
    let mut readback_attempted = request.mode == RunMode::Recover;
    loop {
        if stop.load(Ordering::Acquire) {
            engine.finish(ObservationEnd::Stopped, io.now_ms());
            break;
        }
        if engine.expired(io.now_ms()) {
            engine.finish(engine.deadline_end(io.now_ms()), io.now_ms());
            break;
        }
        if !readback_attempted && engine.snapshot.application != Application::Unconfirmed {
            readback_attempted = true;
            engine.fact(Fact::Phase(Phase::ReadingBack), io.now_ms());
            let mut info = Info::default();
            match console.query(Query::Info(&mut info), io, engine, stop, publish) {
                Ok((start, end)) => {
                    if info.address() != engine.snapshot.identity.as_deref().unwrap_or_default() {
                        engine.fact(Fact::ReadbackIncomplete, io.now_ms());
                        engine.fact(Fact::Application(Application::Conflicting), io.now_ms());
                        engine.fact(
                            Fact::Uncertain("Console identity changed during readback".into()),
                            io.now_ms(),
                        );
                        engine.finish(ObservationEnd::Ambiguous, io.now_ms());
                        break;
                    }
                    engine.fact(Fact::Readback(info.values(start, end)), io.now_ms());
                    publish(&engine.snapshot);
                }
                Err(error) => return finish_query_error(error, io, engine, publish, true),
            }
        } else if let Err(error) = console.read_lines(io, engine, publish) {
            match error {
                QueryError::Io(error) => return Err(error),
                QueryError::Invalid(error) => {
                    engine.fact(Fact::Uncertain(error), io.now_ms());
                    engine.finish(ObservationEnd::Ambiguous, io.now_ms());
                    break;
                }
                QueryError::Conflict(error) => {
                    engine.fact(Fact::Uncertain(error), io.now_ms());
                    engine.fact(Fact::Application(Application::Conflicting), io.now_ms());
                    engine.finish(ObservationEnd::Ambiguous, io.now_ms());
                    break;
                }
                QueryError::Stopped | QueryError::Deadline => unreachable!(),
            }
        }
    }
    publish(&engine.snapshot);
    Ok(())
}

#[cfg(test)]
pub(super) const FIXTURE_HELP: &str = r#"
=== Available Commands ===

Device Information:
  info                       Get device information

Sensor Management:
  calibrate                  Calibrate sensor ZRO
  sens auto <x|y|z> [rev]    Auto-calibrate gyro sensitivity by spinning (default 5 rev)

Connection:
  pair                       Enter pairing mode

System:
  reboot                     Soft reset the device

Other:
  help                       Show this help message

Debug Commands:
  reset all                  Clear all settings

Button Functions (current build):
  No sw0 button is defined for this board

"#;

#[cfg(test)]
pub(super) const FIXTURE_INFO: &str = "\
* SlimeVR Tracker *\n\
SlimeVR synthetic fixture\n\
Repo: https://example.invalid/tracker | Branch: fixture\n\n\
Board: fixture\n\
SOC: nrf52840\n\
Target: fixture/nrf52840\n\n\
IMU: BMI270\n\
Interface: SPI\n\n\
Magnetometer: None (disabled)\n\n\
ODR: gyro 200.0Hz / accel 100.0Hz / mag n/a | fusion 100.0Hz\n\
Gyroscope bias: 0.10000 0.20000 0.30000\n\
Gyroscope sensitivity scale: 1.00000 1.00000 1.00000\n\n\
Fusion: VQF\n\n\
Tracker ID: None\n\
Device address: AABBCCDDEEFF\n\
Receiver address: None\n\
RF Channel: 40 (default)\n\n\
Battery: Waiting for valid unplugged reading\n\
Remaining runtime: Not available\n\
Fully charged runtime: Not available\n";

#[cfg(test)]
mod tests {
    use super::{DirectIo, FIXTURE_HELP, FIXTURE_INFO, run};
    use crate::domain::{SerialEndpoint, SerialPortKind};
    use crate::features::calibration::engine::{
        Application, Axis, CalibrationTask, Dispatch, Engine, Fact, ObservationEnd, Persistence,
        Revolutions, RunMode, RunRequest, Target,
    };
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Scripted {
        now: u64,
        pending: VecDeque<Vec<u8>>,
        writes: Vec<Vec<u8>>,
        help: String,
        before: String,
        after: String,
        operation: String,
        fragment: usize,
        info_queries: usize,
        fail_operation: bool,
        disconnect_readback: bool,
        stop_after_baseline: Option<Arc<AtomicBool>>,
        scheduled: VecDeque<(u64, String)>,
    }

    impl Scripted {
        fn new(operation: impl Into<String>) -> Self {
            Self {
                now: 0,
                pending: VecDeque::new(),
                writes: Vec::new(),
                help: FIXTURE_HELP.into(),
                before: FIXTURE_INFO.into(),
                after: FIXTURE_INFO.into(),
                operation: operation.into(),
                fragment: usize::MAX,
                info_queries: 0,
                fail_operation: false,
                disconnect_readback: false,
                stop_after_baseline: None,
                scheduled: VecDeque::new(),
            }
        }
    }

    impl DirectIo for Scripted {
        fn now_ms(&self) -> u64 {
            self.now
        }

        fn read(&mut self, bytes: &mut [u8]) -> Result<usize, String> {
            if self.pending.is_empty()
                && self
                    .scheduled
                    .front()
                    .is_some_and(|(at, _)| *at <= self.now)
            {
                let (_, response) = self.scheduled.pop_front().unwrap();
                self.pending.push_back(response.into_bytes());
            }
            if self.disconnect_readback && self.info_queries == 2 && self.pending.is_empty() {
                return Err("fixture unplugged during readback".into());
            }
            let Some(mut next) = self.pending.pop_front() else {
                self.now += 100;
                return Ok(0);
            };
            let length = next.len().min(bytes.len()).min(self.fragment);
            bytes[..length].copy_from_slice(&next[..length]);
            next.drain(..length);
            if !next.is_empty() {
                self.pending.push_front(next);
            }
            self.now += 1;
            if self.pending.is_empty()
                && self.info_queries == 1
                && let Some(stop) = &self.stop_after_baseline
            {
                stop.store(true, Ordering::Release);
            }
            Ok(length)
        }

        fn write_command(&mut self, bytes: &[u8], _: &AtomicBool) -> Result<(), String> {
            self.writes.push(bytes.to_vec());
            let response = match bytes {
                b"help\n" => self.help.clone(),
                b"info\n" => {
                    self.info_queries += 1;
                    if self.info_queries == 1 {
                        self.before.clone()
                    } else if self.disconnect_readback {
                        String::new()
                    } else {
                        self.after.clone()
                    }
                }
                _ if self.fail_operation => return Err("fixture possible partial transfer".into()),
                _ => self.operation.clone(),
            };
            if !response.is_empty() {
                self.pending.push_back(response.into_bytes());
            }
            Ok(())
        }
    }

    fn request(task: CalibrationTask) -> RunRequest {
        RunRequest {
            id: 1,
            target: Target::Direct {
                endpoint: SerialEndpoint {
                    port_name: "fixture-console".into(),
                    label: "Synthetic tracker".into(),
                    manufacturer: None,
                    product: None,
                    kind: SerialPortKind::Usb,
                    serial_number: Some("SYNTHETIC".into()),
                    vid: Some(0x1209),
                    pid: Some(1),
                    usb_interface: None,
                },
                generation: Some(1),
                address: Some("AABBCCDDEEFF".into()),
            },
            task,
            mode: RunMode::Execute,
            recovery: None,
        }
    }

    fn sensitivity(axis: Axis, revolutions: u16) -> CalibrationTask {
        CalibrationTask::Sensitivity {
            axis,
            revolutions: Revolutions::new(revolutions).unwrap(),
        }
    }

    fn applied(axis: &str, revolutions: u16) -> String {
        format!(
            "Gyro sensitivity auto-calibration started on {axis} axis ({revolutions} rev).\n\
             Gyro sensitivity auto-calibration applied: axis {axis}, scale 1.00000, equivalent sens diff over 5 rev: 0.000 deg, off-axis 0.00.\n"
        )
    }

    fn execute(request: &RunRequest, io: &mut Scripted) -> Engine {
        let mut engine = Engine::new(request.clone());
        run(request, io, &mut engine, &AtomicBool::new(false), &|_| {}).unwrap();
        engine
    }

    #[test]
    fn stationary_queue_is_not_application_or_storage_proof() {
        let mut io = Scripted::new(
            "[00:00:01.000,000] <inf> cal_imu: Reading data\n\
             [00:00:02.000,000] <inf> cal_imu: Gyroscope bias: 9.00000 8.00000 7.00000\n\
             [00:00:02.000,001] <inf> cal_imu: Calibration queued for next sensor frame\n",
        );
        let engine = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(
            io.writes,
            [
                b"help\n".to_vec(),
                b"info\n".to_vec(),
                b"calibrate\n".to_vec(),
                b"info\n".to_vec()
            ]
        );
        assert_eq!(engine.snapshot.application, Application::CandidateQueued);
        assert_eq!(engine.snapshot.persistence, Persistence::Unknown);
        assert_eq!(
            engine.snapshot.baseline.as_ref().unwrap().bias,
            Some([0.1, 0.2, 0.3])
        );
        assert_eq!(
            engine.snapshot.readback.as_ref().unwrap().bias,
            Some([0.1, 0.2, 0.3])
        );
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Complete));
    }

    #[test]
    fn sensitivity_axes_and_explicit_revolution_bounds_write_once() {
        for (axis, label, revolutions) in
            [(Axis::X, "X", 1), (Axis::Y, "Y", 5), (Axis::Z, "Z", 100)]
        {
            let mut io = Scripted::new(applied(label, revolutions));
            let engine = execute(&request(sensitivity(axis, revolutions)), &mut io);
            assert_eq!(
                io.writes,
                [
                    b"help\n".to_vec(),
                    b"info\n".to_vec(),
                    format!("sens auto {} {revolutions}\n", label.to_ascii_lowercase())
                        .into_bytes(),
                    b"info\n".to_vec(),
                ]
            );
            assert_eq!(engine.snapshot.application, Application::Applied);
            assert_eq!(engine.snapshot.persistence, Persistence::Unknown);
            assert_eq!(
                engine.snapshot.readback.as_ref().unwrap().scales,
                Some([1.0, 1.0, 1.0])
            );
        }
    }

    #[test]
    fn every_byte_fragment_crlf_ansi_and_utf8_preserve_query_scope() {
        let mut io = Scripted::new(applied("Y", 5));
        io.fragment = 1;
        io.help = io
            .help
            .lines()
            .map(|line| format!("\x1b[32m{line}\x1b[0m\r\n"))
            .collect();
        io.before = io.before.replace(
            "Board: fixture",
            "Board: fixture\n[00:00:00] <inf> other: 界",
        );
        io.after = io.after.replace(
            "Fusion: VQF",
            "[00:00:03] <inf> cal_imu: Gyroscope bias: 9.00000 8.00000 7.00000\nFusion: VQF",
        );
        let engine = execute(&request(sensitivity(Axis::Y, 5)), &mut io);
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert_eq!(
            engine.snapshot.readback.as_ref().unwrap().bias,
            Some([0.1, 0.2, 0.3])
        );
        assert_eq!(io.writes.len(), 4);
    }

    #[test]
    fn unchanged_target_and_non_target_bias_discrepancies_remain_observations() {
        let mut io = Scripted::new(applied("Y", 5));
        io.after = io
            .after
            .replace(
                "scale: 1.00000 1.00000 1.00000",
                "scale: 1.00001 1.00000 0.99999",
            )
            .replace(
                "bias: 0.10000 0.20000 0.30000",
                "bias: 0.10001 0.20000 0.30000",
            );
        let engine = execute(&request(sensitivity(Axis::Y, 5)), &mut io);
        let before = engine.snapshot.baseline.as_ref().unwrap();
        let after = engine.snapshot.readback.as_ref().unwrap();
        assert_eq!(before.scales, Some([1.0, 1.0, 1.0]));
        assert_eq!(after.scales, Some([1.00001, 1.0, 0.99999]));
        assert_eq!(after.bias, Some([0.10001, 0.2, 0.3]));
        assert!(after.query_start_ms > before.query_end_ms);
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert_eq!(io.writes.len(), 4);
    }

    #[test]
    fn unsupported_incomplete_conflicting_identity_and_nonfinite_info_do_not_dispatch() {
        let variants = [
            FIXTURE_INFO.replace("* SlimeVR Tracker *", "* SlimeVR Receiver *"),
            FIXTURE_INFO.replace("* SlimeVR Tracker *", "Unknown console"),
            FIXTURE_INFO.replace("AABBCCDDEEFF", "112233445566"),
            FIXTURE_INFO.replace("SOC: nrf52840\n", ""),
            FIXTURE_INFO.replace(
                "Device address: AABBCCDDEEFF",
                "Device address: AABBCCDDEEFF\nDevice address: 112233445566",
            ),
            FIXTURE_INFO.replace("0.10000 0.20000 0.30000", "nan 0.20000 0.30000"),
            FIXTURE_INFO.replace("Fully charged runtime: Not available\n", ""),
        ];
        for before in variants {
            let mut io = Scripted::new(applied("X", 5));
            io.before = before;
            let engine = execute(&request(sensitivity(Axis::X, 5)), &mut io);
            assert_eq!(io.writes, [b"help\n".to_vec(), b"info\n".to_vec()]);
            assert_eq!(engine.snapshot.dispatch, Dispatch::NotAttempted);
            assert_eq!(engine.snapshot.application, Application::Unconfirmed);
            assert!(engine.snapshot.end.is_some());
        }
    }

    #[test]
    fn incomplete_help_is_quarantined_without_next_query() {
        let mut io = Scripted::new(applied("X", 5));
        io.help = FIXTURE_HELP.trim_end().to_owned() + "\n";
        let engine = execute(&request(sensitivity(Axis::X, 5)), &mut io);
        assert_eq!(io.writes, [b"help\n".to_vec()]);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Unsupported));
        assert_eq!(engine.snapshot.dispatch, Dispatch::NotAttempted);
    }

    #[test]
    fn missing_sens_advertisement_rejects_without_operation() {
        let mut io = Scripted::new(applied("X", 5));
        io.help = io
            .help
            .lines()
            .filter(|line| !line.contains("sens auto"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let engine = execute(&request(sensitivity(Axis::X, 5)), &mut io);
        assert_eq!(io.writes, [b"help\n".to_vec(), b"info\n".to_vec()]);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Unsupported));
        assert!(!engine.snapshot.sensitivity_supported);
    }

    #[test]
    fn stop_after_preparation_never_dispatches_and_inspect_only_queries() {
        let stop = Arc::new(AtomicBool::new(false));
        let request = request(CalibrationTask::Stationary);
        let mut engine = Engine::new(request.clone());
        let mut io = Scripted::new("");
        io.stop_after_baseline = Some(Arc::clone(&stop));
        run(&request, &mut io, &mut engine, &stop, &|_| {}).unwrap();
        assert_eq!(io.writes, [b"help\n".to_vec(), b"info\n".to_vec()]);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Stopped));
        let mut inspection = request.clone();
        inspection.mode = RunMode::Inspect;
        let mut io = Scripted::new("");
        let inspected = execute(&inspection, &mut io);
        assert_eq!(io.writes, [b"help\n".to_vec(), b"info\n".to_vec()]);
        assert_eq!(inspected.snapshot.identity.as_deref(), Some("AABBCCDDEEFF"));
        assert_eq!(inspected.snapshot.end, Some(ObservationEnd::Complete));
    }

    #[test]
    fn possible_partial_operation_write_is_never_replayed() {
        let request = request(CalibrationTask::Stationary);
        let mut io = Scripted::new("");
        io.fail_operation = true;
        let mut engine = Engine::new(request.clone());
        let error = run(
            &request,
            &mut io,
            &mut engine,
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap_err();
        assert_eq!(error, "fixture possible partial transfer");
        assert_eq!(
            io.writes,
            [
                b"help\n".to_vec(),
                b"info\n".to_vec(),
                b"calibrate\n".to_vec()
            ]
        );
        assert_eq!(engine.snapshot.dispatch, Dispatch::Attempted);
        assert_eq!(engine.snapshot.application, Application::Unconfirmed);
    }

    #[test]
    fn readback_disconnect_does_not_erase_applied_evidence() {
        let request = request(sensitivity(Axis::X, 5));
        let mut io = Scripted::new(applied("X", 5));
        io.disconnect_readback = true;
        let mut engine = Engine::new(request.clone());
        assert_eq!(
            run(
                &request,
                &mut io,
                &mut engine,
                &AtomicBool::new(false),
                &|_| {}
            )
            .unwrap_err(),
            "fixture unplugged during readback"
        );
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert!(engine.snapshot.readback_incomplete);
        assert_eq!(io.writes.len(), 4);
    }

    #[test]
    fn storage_before_or_after_applied_is_sticky_and_unrelated_logs_are_not_terminals() {
        for earlier in [true, false] {
            let storage = "[00:00:02] <err> cal_sens: Sensitivity calibration applied in RAM; persistence failed: -5\n";
            let terminal = applied("Z", 5);
            let operation = if earlier {
                format!("{storage}{terminal}")
            } else {
                format!("{terminal}{storage}")
            };
            let mut io = Scripted::new(operation);
            io.after
                .push_str("[00:00:03] <err> bluetooth: failed applied success\n");
            let engine = execute(&request(sensitivity(Axis::Z, 5)), &mut io);
            assert_eq!(engine.snapshot.application, Application::Applied);
            assert_eq!(engine.snapshot.persistence, Persistence::StorageError);
            assert_eq!(io.writes.len(), 4);
        }
    }

    #[test]
    fn wrong_axis_duplicate_start_and_conflicting_applied_results_are_ambiguous() {
        for operation in [
            applied("Y", 5),
            format!(
                "{}Gyro sensitivity auto-calibration started on X axis (5 rev).\n",
                applied("X", 5)
            ),
            format!(
                "{}{}",
                applied("X", 5),
                applied("X", 5).replace("scale 1.00000", "scale 1.00100")
            ),
        ] {
            let mut io = Scripted::new(operation);
            let engine = execute(&request(sensitivity(Axis::X, 5)), &mut io);
            assert_eq!(engine.snapshot.application, Application::Conflicting);
            assert_eq!(engine.snapshot.end, Some(ObservationEnd::Ambiguous));
            assert_eq!(
                io.writes
                    .iter()
                    .filter(|bytes| bytes.starts_with(b"sens auto"))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn oversized_query_and_partial_prior_input_close_without_dispatch() {
        let mut io = Scripted::new("");
        io.help = format!("{}\n", "x".repeat(2_049));
        let engine = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Unsupported));
        assert_eq!(io.writes, [b"help\n".to_vec()]);
        let mut io = Scripted::new("");
        io.pending.push_back(b"old incomplete response".to_vec());
        let engine = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Unsupported));
        assert!(io.writes.is_empty());
    }

    #[test]
    fn applied_before_line_overflow_is_preserved() {
        let mut io = Scripted::new(format!("{}{}\n", applied("X", 5), "x".repeat(2_049)));
        let engine = execute(&request(sensitivity(Axis::X, 5)), &mut io);
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Ambiguous));
        assert_eq!(io.writes.len(), 3);
    }

    #[test]
    fn response_byte_line_and_barrier_saturation_limits_fail_closed() {
        for response in [
            format!(
                "{}\n{}",
                "[00:00:00] <inf> other: diagnostic".repeat(2),
                "\n".repeat(1_025)
            ),
            (0..90)
                .map(|_| format!("[00:00:00] <inf> other: {}\n", "x".repeat(900)))
                .collect::<String>(),
        ] {
            let mut io = Scripted::new("");
            io.help = response;
            let engine = execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(io.writes, [b"help\n".to_vec()]);
            assert_eq!(engine.snapshot.end, Some(ObservationEnd::Unsupported));
        }
        let mut io = Scripted::new("");
        io.pending.push_back(
            (0..90)
                .map(|_| format!("[00:00:00] <inf> other: {}\n", "x".repeat(900)))
                .collect::<String>()
                .into_bytes(),
        );
        let engine = execute(&request(CalibrationTask::Stationary), &mut io);
        assert!(io.writes.is_empty());
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Unsupported));
    }

    #[test]
    fn incomplete_post_query_keeps_applied_and_never_sends_another_query() {
        let mut io = Scripted::new(applied("X", 5));
        io.after = io
            .after
            .replace("Fully charged runtime: Not available\n", "");
        let engine = execute(&request(sensitivity(Axis::X, 5)), &mut io);
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert!(engine.snapshot.readback_incomplete);
        assert!(engine.snapshot.readback.is_none());
        assert_eq!(io.writes.len(), 4);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Ambiguous));
    }

    #[test]
    fn late_storage_and_duplicate_applied_do_not_extend_fixed_tail() {
        let mut io = Scripted::new(applied("X", 5));
        io.scheduled.push_back((19_000, "Gyro sensitivity auto-calibration applied: axis X, scale 1.00000, equivalent sens diff over 5 rev: 0.000 deg, off-axis 0.00.\n".into()));
        io.scheduled.push_back((19_100, "[00:00:19] <err> cal_sens: Sensitivity calibration applied in RAM; persistence failed: -5\n".into()));
        let engine = execute(&request(sensitivity(Axis::X, 5)), &mut io);
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert_eq!(engine.snapshot.persistence, Persistence::StorageError);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Complete));
        assert!((20_000..21_000).contains(&io.now));
        assert_eq!(io.writes.len(), 4);
    }

    #[test]
    fn admission_rejection_differs_from_algorithm_quality_rejection() {
        let mut io = Scripted::new("Error: Calibration request rejected: -16.\n");
        let engine = execute(&request(sensitivity(Axis::X, 5)), &mut io);
        assert_eq!(engine.snapshot.dispatch, Dispatch::Rejected);
        assert_eq!(engine.snapshot.application, Application::Rejected);
        let mut io = Scripted::new(
            "Gyro sensitivity auto-calibration started on X axis (5 rev).\n\
             Gyro sensitivity auto-calibration rejected: too much off-axis motion (0.50 > 0.25). Nothing saved.\n",
        );
        let engine = execute(&request(sensitivity(Axis::X, 5)), &mut io);
        assert_eq!(engine.snapshot.dispatch, Dispatch::ConsoleAccepted);
        assert_eq!(engine.snapshot.application, Application::Rejected);
    }

    #[test]
    fn source_profile_scale_log_and_loose_alignment_suffix_are_supported() {
        let mut io = Scripted::new(
            "Gyro sensitivity auto-calibration started on Y axis (5 rev).\n\
             [00:00:01] <inf> cal_sens: Sensitivity calibration: axis Y, 5 rev (1800.0 deg expected)\n\
             [00:00:02] <inf> cal_sens: Sensitivity calibration: hold still\n\
             [00:00:03] <inf> cal_sens: Sensitivity calibration: spin the tracker about the Y axis now\n\
             [00:00:04] <inf> cal_sens: Sensitivity calibration: recording\n\
             [00:00:05] <inf> cal_sens: Sensitivity calibration: computed scale 1.00000\n\
             [00:00:05] <inf> cal_sens: Sensitivity calibration: axis Y scale set to 1.00000\n\
             Gyro sensitivity auto-calibration applied: axis Y, scale 1.00000, equivalent sens diff over 5 rev: 0.000 deg, off-axis 0.15. Axis alignment was loose; repeating may improve accuracy.\n",
        );
        let engine = execute(&request(sensitivity(Axis::Y, 5)), &mut io);
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert_eq!(engine.snapshot.dispatch, Dispatch::ConsoleAccepted);
        assert_eq!(io.writes.len(), 4);
    }

    #[test]
    fn conflicting_role_or_query_grammar_and_malformed_rates_reject_preparation() {
        for before in [
            FIXTURE_INFO.replace("Target: fixture/nrf52840", "Target: receiver/nrf52840"),
            FIXTURE_INFO.replace("Fusion: VQF", "* SlimeVR Receiver *\nFusion: VQF"),
            FIXTURE_INFO.replace("Fusion: VQF", "=== Available Commands ===\nFusion: VQF"),
            FIXTURE_INFO.replace("gyro 200.0Hz", "gyro nanHz"),
        ] {
            let mut io = Scripted::new("");
            io.before = before;
            let engine = execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(engine.snapshot.dispatch, Dispatch::NotAttempted);
            assert_eq!(engine.snapshot.end, Some(ObservationEnd::Unsupported));
            assert_eq!(io.writes.len(), 2);
        }
    }

    #[test]
    fn stationary_storage_error_is_positive_ram_application_not_saved() {
        let mut io = Scripted::new(
            "[00:00:01] <inf> cal_imu: Calibration queued for next sensor frame\n\
             [00:00:02] <err> imu_calibration: Calibration applied in RAM; persistence failed: -5\n",
        );
        let engine = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert_eq!(engine.snapshot.persistence, Persistence::StorageError);
        assert_eq!(
            io.writes,
            [
                b"help\n".to_vec(),
                b"info\n".to_vec(),
                b"calibrate\n".to_vec(),
                b"info\n".to_vec()
            ]
        );
    }

    #[test]
    fn another_kind_progress_without_plain_start_is_conflicting() {
        let mut io =
            Scripted::new("[00:00:01] <inf> cal_sens: Sensitivity calibration: hold still\n");
        let engine = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(engine.snapshot.application, Application::Conflicting);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Ambiguous));
        assert_eq!(
            io.writes,
            [
                b"help\n".to_vec(),
                b"info\n".to_vec(),
                b"calibrate\n".to_vec()
            ]
        );
    }

    #[test]
    fn recovery_queries_and_observes_late_storage_without_an_operation_write() {
        for during_query in [true, false] {
            let mut request = request(CalibrationTask::Stationary);
            request.mode = RunMode::Recover;
            let mut io = Scripted::new("");
            io.help = format!("[00:00:00] <inf> cal_imu: Reading data\n{}", io.help);
            let storage = "[00:00:01] <err> imu_calibration: Calibration applied in RAM; persistence failed: -5\n";
            if during_query {
                io.before = io
                    .before
                    .replace("Fusion: VQF", &format!("{storage}Fusion: VQF"));
            } else {
                io.scheduled.push_back((1_000, storage.into()));
            }
            let mut engine = Engine::new(request.clone());
            engine.fact(Fact::NativeOpen(42), 0);
            engine.fact(Fact::Application(Application::Applied), 0);
            run(
                &request,
                &mut io,
                &mut engine,
                &AtomicBool::new(false),
                &|_| {},
            )
            .unwrap();
            assert_eq!(io.writes, [b"help\n".to_vec(), b"info\n".to_vec()]);
            assert_eq!(engine.snapshot.application, Application::Applied);
            assert_eq!(engine.snapshot.persistence, Persistence::StorageError);
            assert_eq!(engine.snapshot.readback.as_ref().unwrap().native_open, 42);
            assert!(io.now <= 30_100);
        }
    }

    #[test]
    fn recovery_does_not_adopt_new_starts_or_wrong_kind_progress() {
        for operation in [
            "Gyro sensitivity auto-calibration started on X axis (5 rev).\n",
            "[00:00:01] <inf> cal_imu: Calibrating main accelerometer and gyroscope zero rate offset\n",
        ] {
            let mut request = request(sensitivity(Axis::X, 5));
            request.mode = RunMode::Recover;
            let mut io = Scripted::new("");
            io.scheduled.push_back((1_000, operation.into()));
            let engine = execute(&request, &mut io);
            assert_eq!(io.writes, [b"help\n".to_vec(), b"info\n".to_vec()]);
            assert_eq!(engine.snapshot.application, Application::Conflicting);
            assert_eq!(engine.snapshot.end, Some(ObservationEnd::Ambiguous));
        }
    }

    #[test]
    fn recovery_requires_fresh_positive_identity_before_attributing_buffered_logs() {
        let mut request = request(CalibrationTask::Stationary);
        request.mode = RunMode::Recover;
        let mut io = Scripted::new("");
        io.before = format!(
            "[00:00:01] <err> imu_calibration: Calibration applied in RAM; persistence failed: -5\n{}",
            FIXTURE_INFO.replace("AABBCCDDEEFF", "112233445566")
        );
        let engine = execute(&request, &mut io);
        assert_eq!(io.writes, [b"help\n".to_vec(), b"info\n".to_vec()]);
        assert_eq!(engine.snapshot.application, Application::Unconfirmed);
        assert_eq!(engine.snapshot.persistence, Persistence::Unknown);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Ambiguous));
    }

    #[test]
    fn quiet_recovery_remains_unknown_and_is_bounded_by_thirty_seconds() {
        let mut request = request(sensitivity(Axis::X, 5));
        request.mode = RunMode::Recover;
        let mut io = Scripted::new("");
        let engine = execute(&request, &mut io);
        assert_eq!(io.writes, [b"help\n".to_vec(), b"info\n".to_vec()]);
        assert_eq!(engine.snapshot.application, Application::Unconfirmed);
        assert_eq!(engine.snapshot.persistence, Persistence::Unknown);
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Deadline));
        assert!((30_000..30_100).contains(&io.now));
    }
}
