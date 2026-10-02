//! `siren`: something that sounds an alarm, e.g. an indoor siren or a smoke alarm's sounder. On
//! or off, sometimes with a choice of tone, a volume and a duration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::InvariantError;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SirenCapabilities {
    /// The tones it can sound. Empty when there's no choice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 256))]
    pub tones: Vec<String>,
    /// Can be told how loud.
    #[serde(default)]
    pub volume: bool,
    /// Can be told for how long.
    #[serde(default)]
    pub duration: bool,
}

impl SirenCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        super::sensor::validate_options(&self.tones)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SirenState {
    pub on: bool,
}

/// Data for `siren.turn_on`: all optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SirenTurnOn {
    /// One of its `tones`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    /// 0.0 (silent) to 1.0 (loudest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub volume_level: Option<f64>,
    /// How long to sound, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 86400))]
    pub duration: Option<u32>,
}

impl SirenTurnOn {
    /// Deserialization of a call runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if let Some(volume) = self.volume_level
            && !(0.0..=1.0).contains(&volume)
        {
            return Err(InvariantError(format!(
                "volume_level {volume} is out of range; it must be from 0.0 to 1.0"
            )));
        }
        if let Some(duration) = self.duration
            && !(1..=86_400).contains(&duration)
        {
            return Err(InvariantError(format!(
                "duration {duration} is out of range; it must be from 1 to 86400 seconds"
            )));
        }
        Ok(())
    }
}

/// Whether a siren can sound as asked. `Err` follows the entity's name.
pub(crate) fn supports(caps: &SirenCapabilities, data: &SirenTurnOn) -> Result<(), String> {
    if let Some(tone) = &data.tone
        && !caps.tones.contains(tone)
    {
        return Err(if caps.tones.is_empty() {
            "has no tones to choose from".to_owned()
        } else {
            format!("has the tones {}, not {tone:?}", caps.tones.join(", "))
        });
    }
    if data.volume_level.is_some() && !caps.volume {
        return Err("can't be told how loud".into());
    }
    if data.duration.is_some() && !caps.duration {
        return Err("can't be told for how long".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_siren_sounds_only_as_it_can() {
        let hall = SirenCapabilities {
            tones: vec!["alarm".into(), "doorbell".into()],
            volume: true,
            duration: false,
        };
        let loud = SirenTurnOn {
            tone: Some("alarm".into()),
            volume_level: Some(0.8),
            duration: None,
        };
        assert!(supports(&hall, &loud).is_ok());
        let long = SirenTurnOn {
            duration: Some(30),
            ..SirenTurnOn::default()
        };
        assert_eq!(
            supports(&hall, &long),
            Err("can't be told for how long".to_owned())
        );
        let shrill = SirenTurnOn {
            volume_level: Some(1.5),
            ..SirenTurnOn::default()
        };
        assert!(shrill.validate().is_err());
    }
}
