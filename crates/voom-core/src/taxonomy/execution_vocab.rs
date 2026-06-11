use crate::VoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Local,
    Remote,
    Synthetic,
}

impl NodeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Synthetic => "synthetic",
        }
    }

    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "local" => Some(Self::Local),
            "remote" => Some(Self::Remote),
            "synthetic" => Some(Self::Synthetic),
            _ => None,
        }
    }

    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value)
            .ok_or_else(|| VoomError::database(format!("{field} {value:?} not in node kind vocab")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Registered,
    Active,
    Stale,
    Retired,
}

impl NodeStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Retired => "retired",
        }
    }

    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "registered" => Some(Self::Registered),
            "active" => Some(Self::Active),
            "stale" => Some(Self::Stale),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }

    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value).ok_or_else(|| {
            VoomError::database(format!("{field} {value:?} not in node status vocab"))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerKind {
    Synthetic,
    Local,
    Remote,
}

impl WorkerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }

    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "synthetic" => Some(Self::Synthetic),
            "local" => Some(Self::Local),
            "remote" => Some(Self::Remote),
            _ => None,
        }
    }

    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value).ok_or_else(|| {
            VoomError::database(format!("{field} {value:?} not in worker kind vocab"))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Registered,
    Active,
    Stale,
    Retired,
}

impl WorkerStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Retired => "retired",
        }
    }

    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "registered" => Some(Self::Registered),
            "active" => Some(Self::Active),
            "stale" => Some(Self::Stale),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }

    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value).ok_or_else(|| {
            VoomError::database(format!("{field} {value:?} not in worker status vocab"))
        })
    }
}

#[cfg(test)]
#[path = "execution_vocab_test.rs"]
mod tests;
