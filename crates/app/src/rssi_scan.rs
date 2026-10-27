//! Read-only observations of the receiver's RSSI console table.
//!
//! The firmware has no end marker. Snapshots describe only validated data received
//! so far; neither a row count nor a period of silence proves scan completion.

use std::sync::Arc;

pub(crate) mod ansi;
mod grammar;

#[cfg(test)]
mod tests;

const MAX_LINE_BYTES: usize = 4096;
const MAX_LOG_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub channel: u8,
    pub rssi_dbm: i16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recommendation {
    Unknown,
    None,
    Channel(Sample),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanReport {
    pub channel_range: (u8, u8),
    pub current_channel: Option<u8>,
    pub recommendation: Recommendation,
    /// Unique, numerically sorted channels. Missing channels have no samples.
    pub samples: Vec<Sample>,
    pub excluded_channels: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ScanSnapshot {
    pub scan_id: u64,
    pub report: Arc<ScanReport>,
}

struct ActiveScan {
    id: u64,
    sweeps: u32,
    table_seen: bool,
    emitted: bool,
    report: Arc<ScanReport>,
}

#[derive(Default)]
pub struct ScanDecoder {
    ansi: ansi::AnsiFilter,
    scan_id: u64,
    active: Option<ActiveScan>,
    latest_error: Option<&'static str>,
}

impl ScanDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one complete console line, without introducing a second byte framer.
    ///
    /// A malformed recognized frame or conflicting value invalidates this scan
    /// until another valid header. Previously delivered snapshots stay immutable,
    /// but no later fragments can amend that invalid scan. Ordinary logs are not
    /// protocol frames and do not invalidate an observation.
    pub fn feed_line(&mut self, line: &str) -> Option<ScanSnapshot> {
        if line.len() > MAX_LINE_BYTES {
            self.discard_line(line.as_bytes());
            return None;
        }
        let line = self.ansi.filter(line);
        let line = line.trim();
        if line.starts_with("RSSI scan (") {
            let Some(header) = grammar::header(line) else {
                self.invalidate("Malformed RSSI scan header");
                return None;
            };
            let Some(scan_id) = self.scan_id.checked_add(1) else {
                self.invalidate("RSSI scan identifier exhausted");
                return None;
            };
            self.scan_id = scan_id;
            self.latest_error = None;
            self.active = Some(ActiveScan {
                id: scan_id,
                sweeps: header.sweeps,
                table_seen: false,
                emitted: false,
                report: Arc::new(ScanReport {
                    channel_range: header.range,
                    current_channel: None,
                    recommendation: Recommendation::Unknown,
                    samples: Vec::new(),
                    excluded_channels: Vec::new(),
                }),
            });
            return None;
        }
        // The console admission gate rejects a second request while the current
        // scan is still running; it does not invalidate that scan's observations.
        if line == "RSSI scan already in progress" {
            if self.active.is_none() {
                self.latest_error = Some("RSSI scan was not admitted");
            }
            return None;
        }
        // Unlike the console gate, radio-owner busy is emitted by a newly
        // admitted attempt before its header, so older data must not be reused.
        if line == "RSSI scan busy: radio in use"
            || line == "RSSI scan could not stop radio"
            || line == "RSSI RADIO/frontend cleanup failed; retaining radio owner"
            || line.starts_with("RSSI scan home radio restore failed:")
        {
            self.invalidate("Receiver reported an RSSI scan failure");
            return None;
        }
        let active = self.active.as_mut()?;
        let changed = match active.observe(line) {
            Ok(changed) => changed,
            Err(()) => {
                self.invalidate("Malformed or conflicting RSSI scan data");
                return None;
            }
        };
        if active.table_seen
            && (!active.report.samples.is_empty()
                || active.report.recommendation == Recommendation::None)
            && (changed || !active.emitted)
        {
            active.emitted = true;
            Some(ScanSnapshot {
                scan_id: active.id,
                report: Arc::clone(&active.report),
            })
        } else {
            None
        }
    }

    fn invalidate(&mut self, reason: &'static str) {
        self.active = None;
        self.latest_error = Some(reason);
    }

    /// Advance control-string state without interpreting discarded console bytes.
    /// Used by the shared framer for oversized or invalid UTF-8 lines.
    pub(crate) fn discard_line(&mut self, bytes: &[u8]) {
        self.ansi.discard(bytes);
        self.invalidate("RSSI console line was discarded");
    }
}

impl ActiveScan {
    fn observe(&mut self, line: &str) -> std::result::Result<bool, ()> {
        if line.starts_with("Current channel (effective):") {
            let value = line
                .strip_prefix("Current channel (effective): ")
                .ok_or(())?;
            let channel = grammar::channel(value).ok_or(())?;
            match self.report.current_channel {
                Some(previous) if previous != channel => return Err(()),
                Some(_) => return Ok(false),
                None => Arc::make_mut(&mut self.report).current_channel = Some(channel),
            }
            return Ok(true);
        }
        if line.starts_with("Recommended:") {
            let recommendation = grammar::recommendation(line).ok_or(())?;
            self.validate_recommendation(recommendation)?;
            if self.report.recommendation == recommendation {
                return Ok(false);
            }
            if self.report.recommendation != Recommendation::Unknown {
                return Err(());
            }
            Arc::make_mut(&mut self.report).recommendation = recommendation;
            return Ok(true);
        }
        if line.starts_with("Channel ") && line.contains(" invalid:") {
            let channel = grammar::excluded(line, self.sweeps).ok_or(())?;
            if !self.in_range(channel)
                || self.sample(channel).is_some()
                || matches!(self.report.recommendation, Recommendation::Channel(sample) if sample.channel == channel)
            {
                return Err(());
            }
            let Err(index) = self.report.excluded_channels.binary_search(&channel) else {
                return Ok(false);
            };
            Arc::make_mut(&mut self.report)
                .excluded_channels
                .insert(index, channel);
            return Ok(true);
        }
        if line == grammar::TABLE_HEADER {
            let changed = !self.table_seen;
            self.table_seen = true;
            return Ok(changed);
        }
        if line.starts_with("All channels sorted by RSSI") {
            return Err(());
        }
        if !self.table_seen || !grammar::looks_like_row(line) {
            return Ok(false);
        }
        // Validate an entire physical row before committing any of its cells.
        let cells = grammar::row(line).ok_or(())?;
        let mut changed = false;
        for (channel, value) in cells.iter().enumerate() {
            let Some(rssi_dbm) = *value else { continue };
            let channel = u8::try_from(channel).map_err(|_| ())?;
            if !self.in_range(channel)
                || self
                    .report
                    .excluded_channels
                    .binary_search(&channel)
                    .is_ok()
                || self.report.recommendation == Recommendation::None
                || self
                    .sample(channel)
                    .is_some_and(|previous| previous != rssi_dbm)
                || matches!(self.report.recommendation, Recommendation::Channel(sample) if sample.channel == channel && sample.rssi_dbm != rssi_dbm)
            {
                return Err(());
            }
            changed |= self.sample(channel).is_none();
        }
        if changed {
            let report = Arc::make_mut(&mut self.report);
            for (channel, value) in cells.into_iter().enumerate() {
                let Some(rssi_dbm) = value else { continue };
                let channel = u8::try_from(channel).map_err(|_| ())?;
                if let Err(index) = report.samples.binary_search_by_key(&channel, |s| s.channel) {
                    report.samples.insert(index, Sample { channel, rssi_dbm });
                }
            }
        }
        Ok(changed)
    }

    fn in_range(&self, channel: u8) -> bool {
        (self.report.channel_range.0..=self.report.channel_range.1).contains(&channel)
    }

    fn sample(&self, channel: u8) -> Option<i16> {
        self.report
            .samples
            .binary_search_by_key(&channel, |sample| sample.channel)
            .ok()
            .map(|index| self.report.samples[index].rssi_dbm)
    }

    fn validate_recommendation(
        &self,
        recommendation: Recommendation,
    ) -> std::result::Result<(), ()> {
        match recommendation {
            Recommendation::None if !self.report.samples.is_empty() => Err(()),
            Recommendation::Channel(sample)
                if !self.in_range(sample.channel)
                    || self
                        .report
                        .excluded_channels
                        .binary_search(&sample.channel)
                        .is_ok()
                    || self
                        .sample(sample.channel)
                        .is_some_and(|value| value != sample.rssi_dbm) =>
            {
                Err(())
            }
            _ => Ok(()),
        }
    }
}

/// Import the latest recognized scan using exactly the live decoder's rules.
/// EOF can terminate a full line, but is not a firmware completion assertion.
pub fn parse_log(text: &str) -> Result<Arc<ScanReport>, String> {
    if text.len() > MAX_LOG_BYTES {
        return Err("RSSI log exceeds the 256 KiB import limit".to_owned());
    }
    let mut decoder = ScanDecoder::new();
    // CRLF yields a harmless empty line; no line-sized allocations are needed.
    for line in text.split(['\r', '\n']) {
        decoder.feed_line(line);
    }
    if let Some(error) = decoder.latest_error {
        return Err(error.to_owned());
    }
    match decoder.active {
        Some(active) if active.emitted => Ok(active.report),
        Some(_) => Err(
            "Latest RSSI scan has no validated measurements or explicit empty result".to_owned(),
        ),
        None => Err("No recognized RSSI scan found in the log".to_owned()),
    }
}
