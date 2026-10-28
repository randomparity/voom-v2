//! Observed sample evidence and bounded integer archival contracts.
//! Integer facts describe the decoded representation, not fidelity lost upstream.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LosslessAudioSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits_per_sample: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioSampleFacts {
    pub sample_rate: Option<u32>,
    pub sample_format: Option<String>,
    pub bits_per_sample: Option<u8>,
    pub channels: Option<u64>,
    pub channel_layout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLosslessAudioSettings {
    pub sample_rate: u32,
    pub channels: u32,
    pub channel_layout: String,
    pub bits_per_sample: u8,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("audio sample facts: {0}")]
pub struct AudioSampleFactsError(String);

impl AudioSampleFacts {
    /// Collect evidence without treating a decoder's storage width as precision.
    ///
    /// # Errors
    /// Returns an error for malformed or contradictory numeric probe facts.
    pub fn from_ffprobe(stream: &Value) -> Result<Self, AudioSampleFactsError> {
        let sample_rate = optional_unsigned(stream, "sample_rate")?
            .map(u32::try_from)
            .transpose()
            .map_err(|_| invalid("sample_rate exceeds u32"))?;
        let raw_bits = optional_unsigned(stream, "bits_per_raw_sample")?
            .map(u8::try_from)
            .transpose()
            .map_err(|_| invalid("raw precision exceeds u8"))?;
        let coded_bits = optional_unsigned(stream, "bits_per_sample")?
            .map(u8::try_from)
            .transpose()
            .map_err(|_| invalid("coded precision exceeds u8"))?;
        if raw_bits
            .zip(coded_bits)
            .is_some_and(|(raw, coded)| raw > coded)
        {
            return Err(invalid("raw precision exceeds coded precision"));
        }
        let bits_per_sample = raw_bits.or(coded_bits);
        let sample_format = optional_string(stream, "sample_fmt")?;
        let capacity = match sample_format.as_deref() {
            Some("s16" | "s16p") => Some(16),
            Some("s32" | "s32p") => Some(32),
            _ => None,
        };
        if capacity.is_some_and(|cap| {
            raw_bits
                .into_iter()
                .chain(coded_bits)
                .any(|bits| bits > cap)
        }) {
            return Err(invalid("precision exceeds decoded sample format"));
        }
        Ok(Self {
            sample_rate,
            sample_format,
            bits_per_sample,
            channels: optional_unsigned(stream, "channels")?,
            channel_layout: optional_string(stream, "channel_layout")?,
        })
    }
}

impl LosslessAudioSettings {
    /// Resolve one source independently. Ordinary archival conversion has no remix/rate knob.
    ///
    /// # Errors
    /// Rejects unknown facts, unsupported formats/layouts, and precision reduction.
    pub fn resolve(
        &self,
        source: &AudioSampleFacts,
    ) -> Result<ResolvedLosslessAudioSettings, AudioSampleFactsError> {
        let sample_rate = source
            .sample_rate
            .filter(|rate| *rate > 0)
            .ok_or_else(|| invalid("positive sample_rate is required"))?;
        let source_bits = source
            .bits_per_sample
            .filter(|bits| matches!(bits, 16 | 24))
            .ok_or_else(|| invalid("known 16/24-bit integer precision is required"))?;
        if !matches!(
            (source.sample_format.as_deref(), source_bits),
            (Some("s16" | "s16p"), 16) | (Some("s32" | "s32p"), 16 | 24)
        ) {
            return Err(invalid(
                "supported signed integer sample_format is required",
            ));
        }
        let layout = source.channel_layout.as_deref();
        if !matches!(
            (source.channels, layout),
            (Some(1), Some("mono"))
                | (Some(2), Some("stereo"))
                | (Some(6), Some("5.1" | "5.1(side)"))
                | (Some(8), Some("7.1"))
        ) {
            return Err(invalid(
                "known matching mono/stereo/5.1/7.1 channel layout is required",
            ));
        }
        let bits_per_sample = self.bits_per_sample.unwrap_or(source_bits);
        if !matches!(bits_per_sample, 16 | 24) || bits_per_sample < source_bits {
            return Err(invalid(
                "target precision must be 16/24 bits without reduction",
            ));
        }
        Ok(ResolvedLosslessAudioSettings {
            sample_rate,
            channels: u32::try_from(source.channels.unwrap_or_default())
                .map_err(|_| invalid("channels exceeds u32"))?,
            channel_layout: layout.unwrap_or_default().to_owned(),
            bits_per_sample,
        })
    }
}

impl ResolvedLosslessAudioSettings {
    /// Verify actual output facts independently of the encoder's requested settings.
    ///
    /// # Errors
    /// Rejects missing, invalid or different rate/layout/precision facts.
    pub fn verify(&self, output: &AudioSampleFacts) -> Result<(), AudioSampleFactsError> {
        let actual = LosslessAudioSettings::default().resolve(output)?;
        if actual != *self {
            return Err(invalid(
                "output rate/channels/layout/precision differs from resolved settings",
            ));
        }
        Ok(())
    }
}

fn optional_unsigned(stream: &Value, key: &str) -> Result<Option<u64>, AudioSampleFactsError> {
    let Some(value) = stream.get(key) else {
        return Ok(None);
    };
    let parsed = match value {
        Value::String(raw) if unknown(raw) => return Ok(None),
        Value::String(raw) => raw.parse::<u64>().ok(),
        _ => value.as_u64(),
    }
    .ok_or_else(|| invalid(format!("{key} must be an unsigned integer")))?;
    Ok((parsed > 0).then_some(parsed))
}

fn optional_string(stream: &Value, key: &str) -> Result<Option<String>, AudioSampleFactsError> {
    let Some(value) = stream.get(key) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| invalid(format!("{key} must be a string")))?;
    Ok((!unknown(value)).then(|| value.to_owned()))
}

fn unknown(value: &str) -> bool {
    matches!(value.trim(), "" | "N/A" | "n/a" | "unknown")
}

fn invalid(message: impl Into<String>) -> AudioSampleFactsError {
    AudioSampleFactsError(message.into())
}

#[cfg(test)]
#[path = "audio_facts_test.rs"]
mod tests;
