//! `IssueSeverity` / `IssuePriority` — enum forms of the
//! `issues.severity` / `issues.priority` TEXT columns defined by §10.2
//! of the Sprint 1 spec. Living in `voom-core` keeps every consumer
//! (`voom-core::failure`, `voom-events` payloads, `voom-store::repo::
//! issues`, the CLI surface) on a single shared type so the wire
//! vocabulary cannot diverge.

use crate::error::VoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl IssueSeverity {
    /// Wire-format string for the `issues.severity` TEXT column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Info => "info",
        }
    }

    /// Parse a TEXT column value back to the enum.
    ///
    /// # Errors
    /// Returns `VoomError::Database` if the string is not in the vocab.
    pub fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "critical" => Ok(Self::Critical),
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            "info" => Ok(Self::Info),
            other => Err(VoomError::Database(format!(
                "issues.severity {other:?} not in vocab"
            ))),
        }
    }
}

impl std::str::FromStr for IssueSeverity {
    type Err = VoomError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssuePriority {
    Urgent,
    High,
    Normal,
    Low,
    Someday,
}

impl IssuePriority {
    /// Wire-format string for the `issues.priority` TEXT column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Urgent => "urgent",
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
            Self::Someday => "someday",
        }
    }

    /// Parse a TEXT column value back to the enum.
    ///
    /// # Errors
    /// Returns `VoomError::Database` if the string is not in the vocab.
    pub fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "urgent" => Ok(Self::Urgent),
            "high" => Ok(Self::High),
            "normal" => Ok(Self::Normal),
            "low" => Ok(Self::Low),
            "someday" => Ok(Self::Someday),
            other => Err(VoomError::Database(format!(
                "issues.priority {other:?} not in vocab"
            ))),
        }
    }
}

impl std::str::FromStr for IssuePriority {
    type Err = VoomError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
#[path = "issue_test.rs"]
mod tests;
