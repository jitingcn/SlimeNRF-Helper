use super::Preset;
use crate::i18n::tr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterSpec {
    None,
    Text {
        hint: &'static str,
    },
    Integer {
        min: u32,
        max: u32,
        default: &'static str,
    },
    OptionalInteger {
        min: u32,
        max: u32,
        hint: &'static str,
    },
    Sensitivity {
        remote: bool,
    },
    SensitivityAuto,
    RemoteReset,
}

impl ParameterSpec {
    pub fn default_value(self) -> &'static str {
        match self {
            Self::None | Self::Text { .. } | Self::OptionalInteger { .. } => "",
            Self::Sensitivity { .. } => "0,0,0",
            Self::SensitivityAuto => "x",
            Self::RemoteReset => "zro",
            Self::Integer { default, .. } => default,
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::None => "commands.parameter.hint.none",
            Self::Text { hint } => hint,
            Self::Integer { .. } => "commands.parameter.hint.channel",
            Self::OptionalInteger { hint, .. } => hint,
            Self::Sensitivity { .. } => "commands.parameter.hint.sensitivity",
            Self::SensitivityAuto => "commands.parameter.hint.sensitivity_auto",
            Self::RemoteReset => "commands.parameter.hint.reset",
        }
    }

    fn validate(self, value: &str) -> Result<&str, String> {
        // Check before trimming: even a trailing newline must not be accepted.
        if value
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            return Err(tr("commands.parameter.error.single_line"));
        }
        let value = value.trim();
        match self {
            Self::None if !value.is_empty() => Err(tr("commands.parameter.error.not_accepted")),
            Self::None => Ok(value),
            Self::OptionalInteger { .. } if value.is_empty() => Ok(value),
            _ if value.is_empty() => Err(tr("commands.parameter.error.required")),
            Self::Text { .. } => Ok(value),
            Self::Integer { min, max, .. } | Self::OptionalInteger { min, max, .. } => {
                validate_integer(value, min, max)?;
                Ok(value)
            }
            Self::SensitivityAuto => {
                let mut tokens = value.split(' ').filter(|token| !token.is_empty());
                if !matches!(tokens.next(), Some("x" | "y" | "z")) {
                    return Err(tr("commands.parameter.error.axis"));
                }
                if let Some(revolutions) = tokens.next() {
                    validate_integer(revolutions, 1, 100)?;
                }
                if tokens.next().is_some() {
                    return Err(tr("commands.parameter.error.axis"));
                }
                Ok(value)
            }
            Self::RemoteReset => {
                if matches!(value, "zro" | "acc" | "bat" | "mag" | "tcal" | "fusion") {
                    Ok(value)
                } else {
                    Err(tr("commands.parameter.error.reset_target"))
                }
            }
            Self::Sensitivity { remote } => {
                if value == "reset" {
                    return Ok(value);
                }
                let error = if remote {
                    "commands.parameter.error.remote_sensitivity"
                } else {
                    "commands.parameter.error.sensitivity"
                };
                let mut values = value.split(',');
                // Only the remote path packs signed centidegrees. Validate its
                // decimal bounds before scaling; f32 scaling would reject 327.67.
                for _ in 0..3 {
                    let valid = values.next().is_some_and(|token| {
                        !token.is_empty()
                            && !token.chars().any(char::is_whitespace)
                            && token.parse::<f64>().is_ok_and(|number| {
                                number.is_finite()
                                    && (number as f32).is_finite()
                                    && (!remote || (-327.68..=327.67).contains(&number))
                            })
                    });
                    if !valid {
                        return Err(tr(error));
                    }
                }
                if values.next().is_some() {
                    return Err(tr(error));
                }
                Ok(value)
            }
        }
    }
}

fn validate_integer(value: &str, min: u32, max: u32) -> Result<(), String> {
    if !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value
            .parse::<u32>()
            .is_ok_and(|number| (min..=max).contains(&number))
    {
        Ok(())
    } else {
        Err(rust_i18n::t!(
            "commands.parameter.error.integer_range",
            min = min,
            max = max
        )
        .to_string())
    }
}

/// Inspect only the command verb, not a shell grammar. Accept either a suffix
/// or our already-frozen `send <target> ...` envelope for route revalidation.
pub(super) fn receiver_wide_payload(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    let mut verb = tokens.next().unwrap_or("");
    if verb.eq_ignore_ascii_case("send") {
        tokens.next();
        verb = tokens.next().unwrap_or("");
    }
    verb.eq_ignore_ascii_case("channel") || verb.eq_ignore_ascii_case("clearchannel")
}

impl Preset {
    pub fn build(&self, parameter: &str) -> Result<String, String> {
        self.build_with_slot(parameter, None)
    }

    pub(super) fn build_with_slot(
        &self,
        parameter: &str,
        slot: Option<u8>,
    ) -> Result<String, String> {
        use std::fmt::Write as _;

        let parameter = self.parameter.validate(parameter)?;
        let suffix = if self.command.trim().is_empty() {
            parameter
        } else {
            self.command
        };
        if slot.is_some() && (self.receiver_wide_only || receiver_wide_payload(suffix)) {
            return Err(tr("commands.authorization.receiver_wide_required"));
        }
        let prefix_len = slot.map_or(0, |slot| if slot < 10 { 7 } else { 8 });
        let mut command = String::with_capacity(
            prefix_len + self.command.len() + parameter.len() + usize::from(!parameter.is_empty()),
        );
        if let Some(slot) = slot {
            write!(command, "send {slot} ").expect("writing to a String cannot fail");
        }
        command.push_str(self.command);
        if !parameter.is_empty() {
            command.push(' ');
            command.push_str(parameter);
        }
        Ok(command)
    }
}
