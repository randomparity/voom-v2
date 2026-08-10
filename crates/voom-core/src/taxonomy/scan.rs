use serde::{Deserialize, Serialize};

use crate::VoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanSessionStatus {
    Requested,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Stale,
}

impl ScanSessionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Stale => "stale",
        }
    }

    /// Parses a persisted scan-session status, reporting unknown values as corruption.
    pub fn parse_database(field: &str, value: String) -> Result<Self, VoomError> {
        match value.as_str() {
            "requested" => Ok(Self::Requested),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "stale" => Ok(Self::Stale),
            _ => Err(VoomError::database(format!(
                "{field} {:?} not in scan session status vocab",
                value.into_boxed_str()
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanTerminalReason(String);

impl ScanTerminalReason {
    pub fn new(value: impl Into<String>) -> Result<Self, VoomError> {
        let value = value.into();
        validate_terminal_reason(&value).map_err(VoomError::Config)?;
        Ok(Self(value))
    }

    pub fn parse_database(field: &str, value: String) -> Result<Self, VoomError> {
        validate_terminal_reason(&value)
            .map_err(|message| VoomError::database(format!("{field} {message}")))?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ScanTerminalReason {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ScanTerminalReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

fn validate_terminal_reason(value: &str) -> Result<(), String> {
    if value
        .trim_matches(|character: char| character.is_ascii_whitespace())
        .is_empty()
    {
        return Err("must not be blank".to_owned());
    }
    if value.len() > 1024 {
        return Err(format!("must be at most 1024 bytes, got {}", value.len()));
    }
    if value.as_bytes().contains(&0) {
        return Err("must not contain NUL bytes".to_owned());
    }
    Ok(())
}

#[cfg(test)]
#[path = "scan_test.rs"]
mod tests;
