//! Source-grounded shortcut commands and frozen, revalidated device routes.
//!
//! Serial vocabulary: official receiver aad8eae3ec8745bbc35c2ca638ac8468c7eed0bc
//! console_send.c and tracker 74e3bd79f16c59e23d1e5d681c8ebed89103a015
//! console.c. Receiver-wide channel operations must never expand a slot selection.
//! Payloads omit LF; the serial transport adds its one terminator.

use crate::domain::DeviceRole;

mod authorization;
mod catalog;
mod parameters;
mod risk;
mod suggestions;

pub(crate) use authorization::CustomCommandSnapshot;
pub use authorization::{CommandSnapshot, RouteEvidence};
use catalog::PRESETS;
pub use catalog::{preset_groups, presets};
pub use parameters::ParameterSpec;
pub use risk::command_tone;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandContext {
    Direct(DeviceRole),
    ForwardedTracker,
}

/// Presentation only: confirmation and route authorization remain independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandTone {
    Normal,
    Warning,
    Destructive,
}

#[derive(Debug)]
pub struct Preset {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub command: &'static str,
    pub parameter: ParameterSpec,
    pub dangerous: bool,
    pub remote_eligible: bool,
    /// Remote channel changes also change the receiver; only explicit all scope is valid.
    pub receiver_wide_only: bool,
    roles: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParameterPreset {
    /// None displays `value` verbatim; Some contains a presentation translation key.
    pub label: Option<&'static str>,
    pub value: &'static str,
}

const TRACKER: u8 = 1;
const RECEIVER: u8 = 2;
const UNKNOWN: u8 = 4;
const BOTH: u8 = TRACKER | RECEIVER;
const ALL: u8 = BOTH | UNKNOWN;

impl Preset {
    pub fn tone(&self) -> CommandTone {
        command_tone(self.command)
    }

    pub fn supports(&self, context: CommandContext) -> bool {
        match context {
            CommandContext::ForwardedTracker => self.remote_eligible,
            CommandContext::Direct(role) => {
                self.roles
                    & match role {
                        DeviceRole::Tracker => TRACKER,
                        DeviceRole::Receiver => RECEIVER,
                        DeviceRole::Unknown => UNKNOWN,
                    }
                    != 0
            }
        }
    }
}

#[cfg(test)]
mod tests;
