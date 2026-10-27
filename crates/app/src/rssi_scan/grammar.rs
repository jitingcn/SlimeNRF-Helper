use super::{Recommendation, Sample};

pub(super) const TABLE_HEADER: &str = "All channels sorted by RSSI (lower is better):";

pub(super) struct Header {
    pub range: (u8, u8),
    pub sweeps: u32,
}

pub(super) fn header(line: &str) -> Option<Header> {
    let mut input = line.strip_prefix("RSSI scan (ESB channels ")?;
    let first = u8::try_from(number(&mut input)?).ok()?;
    take(&mut input, "..")?;
    let last = u8::try_from(number(&mut input)?).ok()?;
    take(&mut input, "): samples/ch/sweep=")?;
    let samples = number(&mut input)?;
    take(&mut input, ", sweeps=")?;
    let sweeps = number(&mut input)?;
    take(&mut input, " (total=")?;
    let total = number(&mut input)?;
    take(&mut input, " samples/ch)")?;
    (input.is_empty()
        && first <= last
        && samples > 0
        && sweeps > 0
        && samples.checked_mul(sweeps) == Some(total))
    .then_some(Header {
        range: (first, last),
        sweeps,
    })
}

pub(super) fn channel(mut input: &str) -> Option<u8> {
    let channel = u8::try_from(number(&mut input)?).ok()?;
    input.is_empty().then_some(channel)
}

pub(super) fn recommendation(line: &str) -> Option<Recommendation> {
    if line == "Recommended: none (no valid complete channel measurements)" {
        return Some(Recommendation::None);
    }
    let mut input = line.strip_prefix("Recommended: channel ")?;
    let sample = cell(&mut input)?;
    input.is_empty().then_some(Recommendation::Channel(sample))
}

pub(super) fn excluded(line: &str, sweeps: u32) -> Option<u8> {
    let mut input = line.strip_prefix("Channel ")?;
    let channel = u8::try_from(number(&mut input)?).ok()?;
    take(&mut input, " invalid: ")?;
    let failed = number(&mut input)?;
    take(&mut input, "/")?;
    let total = number(&mut input)?;
    take(&mut input, " visits failed; excluded")?;
    (input.is_empty() && failed > 0 && failed <= total && total == sweeps).then_some(channel)
}

pub(super) fn looks_like_row(line: &str) -> bool {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && line[digits..].trim_start().starts_with('(')
}

pub(super) fn row(mut input: &str) -> Option<[Option<i16>; 256]> {
    let mut samples = [None; 256];
    loop {
        let sample = cell(&mut input)?;
        let slot = &mut samples[usize::from(sample.channel)];
        if slot.is_some_and(|value| value != sample.rssi_dbm) {
            return None;
        }
        *slot = Some(sample.rssi_dbm);
        if input.is_empty() {
            return Some(samples);
        }
        let rest = input.trim_start_matches([' ', '\t']);
        // A Zephyr log may interrupt between complete printk cells. Never look
        // for measurements inside its payload, even if they resemble cells.
        if log_suffix(rest) {
            return Some(samples);
        }
        if rest.len() == input.len() {
            return None;
        }
        input = rest;
        if input.is_empty() {
            return Some(samples);
        }
    }
}

fn cell(input: &mut &str) -> Option<Sample> {
    let channel = u8::try_from(number(input)?).ok()?;
    let rest = input.trim_start_matches([' ', '\t']);
    if rest.len() == input.len() {
        return None;
    }
    *input = rest;
    take(input, "(-")?;
    *input = input.trim_start_matches([' ', '\t']);
    let magnitude = number(input)?;
    if magnitude > 127 {
        return None;
    }
    take(input, " dBm)")?;
    Some(Sample {
        channel,
        rssi_dbm: -i16::try_from(magnitude).ok()?,
    })
}

fn number(input: &mut &str) -> Option<u32> {
    let digits = input.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let number = input[..digits].parse().ok()?;
    *input = &input[digits..];
    Some(number)
}

fn take(input: &mut &str, prefix: &str) -> Option<()> {
    *input = input.strip_prefix(prefix)?;
    Some(())
}

fn log_suffix(input: &str) -> bool {
    let Some(mut input) = input.strip_prefix('[') else {
        return false;
    };
    // Zephyr's timestamped log grammar, not an arbitrary '[' suffix.
    for separator in [":", ":", ".", ",", "] "] {
        if number(&mut input).is_none() || take(&mut input, separator).is_none() {
            return false;
        }
    }
    let Some(rest) = ["<inf> ", "<dbg> ", "<wrn> ", "<err> "]
        .iter()
        .find_map(|level| input.strip_prefix(level))
    else {
        return false;
    };
    let module = rest
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.'))
        .count();
    module > 0 && rest[module..].starts_with(':')
}
