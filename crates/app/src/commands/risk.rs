use super::{CommandContext, CommandTone, PRESETS};
use crate::domain::DeviceRole;

/// Classify complete command tokens, including typed receiver forwarding.
/// Do not use this visual classification to authorize a command.
pub fn command_tone(command: &str) -> CommandTone {
    let mut tokens = command.split_whitespace();
    let mut first = tokens.next().unwrap_or("");
    if first.eq_ignore_ascii_case("send") {
        tokens.next();
        first = tokens.next().unwrap_or("");
    }
    let second = tokens.next().unwrap_or("");
    let third = tokens.next().unwrap_or("");
    let is = |expected: &str| first.eq_ignore_ascii_case(expected);
    let sub = |expected: &str| second.eq_ignore_ascii_case(expected);
    if is("clear")
        || is("clearchannel")
        || is("remove")
        || is("resetstats")
        || (is("sens") && sub("reset"))
        || (is("tcal") && (sub("clear") || sub("remove")))
        || (is("mag") && sub("clear"))
        || (is("range") && sub("reset"))
        || (is("reset") && !sub("fusion"))
    {
        CommandTone::Destructive
    } else if is("reboot")
        || is("shutdown")
        || is("dfu")
        || is("calibrate")
        || is("6-side")
        || is("reset")
        || (is("mag") && sub("cal"))
        || is("sens")
        || is("channel")
        || is("tdma")
        || (is("test") && sub("on"))
        || (is("tcal") && sub("heat"))
        || (is("wom") && (sub("enter") || (sub("timeout") && !third.is_empty())))
    {
        CommandTone::Warning
    } else {
        CommandTone::Normal
    }
}

pub(super) fn payload_requires_confirmation(command: &str, context: CommandContext) -> bool {
    // A generic parameter may select a more specific dangerous command, e.g.
    // `sens` + `reset`. Match complete tokens, including extra arguments, rather
    // than relying on the clicked entry or a whitespace-sensitive string prefix.
    let payload = command
        .split_whitespace()
        .skip(if context == CommandContext::ForwardedTracker {
            2
        } else {
            0
        });
    PRESETS
        .iter()
        .filter(|preset| {
            preset.dangerous
                && (preset.supports(context)
                    || (context == CommandContext::ForwardedTracker
                        && preset.supports(CommandContext::Direct(DeviceRole::Tracker))))
        })
        .any(|preset| token_prefix_matches(payload.clone(), preset.command))
}

pub(super) fn custom_known_destructive(command: &str) -> bool {
    if command_tone(command) == CommandTone::Destructive {
        return true;
    }
    let mut payload = command.split_whitespace();
    if payload
        .clone()
        .next()
        .is_some_and(|token| token.eq_ignore_ascii_case("send"))
    {
        payload.next();
        payload.next();
    }
    PRESETS
        .iter()
        .filter(|preset| preset.dangerous)
        .any(|preset| token_prefix_matches(payload.clone(), preset.command))
}

fn token_prefix_matches<'a>(mut actual: impl Iterator<Item = &'a str>, prefix: &str) -> bool {
    prefix.split_whitespace().all(|expected| {
        actual
            .next()
            .is_some_and(|token| token.eq_ignore_ascii_case(expected))
    })
}
