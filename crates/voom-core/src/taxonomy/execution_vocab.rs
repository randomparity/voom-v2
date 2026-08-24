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

    /// Parses a stored value from the node-kind vocabulary.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when `value` is outside the node-kind vocabulary.
    /// The error message includes the supplied `field` and `value`.
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

    /// Parses a stored value from the node-status vocabulary.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when `value` is outside the node-status vocabulary.
    /// The error message includes the supplied `field` and `value`.
    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value).ok_or_else(|| {
            VoomError::database(format!("{field} {value:?} not in node status vocab"))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeIncarnationStatus {
    Active,
    Superseded,
    Retired,
    Failed,
}

impl NodeIncarnationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Retired => "retired",
            Self::Failed => "failed",
        }
    }

    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        match value {
            "active" => Ok(Self::Active),
            "superseded" => Ok(Self::Superseded),
            "retired" => Ok(Self::Retired),
            "failed" => Ok(Self::Failed),
            _ => Err(VoomError::database(format!(
                "{field} {value:?} not in node incarnation status vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeIncarnationEndReason {
    Superseded,
    GracefulShutdown,
    ChildStartupFailed,
    ChildRestartExhausted,
    HeartbeatExpired,
    LogicalNodeRetired,
}

impl NodeIncarnationEndReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Superseded => "superseded",
            Self::GracefulShutdown => "graceful_shutdown",
            Self::ChildStartupFailed => "child_startup_failed",
            Self::ChildRestartExhausted => "child_restart_exhausted",
            Self::HeartbeatExpired => "heartbeat_expired",
            Self::LogicalNodeRetired => "logical_node_retired",
        }
    }

    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        match value {
            "superseded" => Ok(Self::Superseded),
            "graceful_shutdown" => Ok(Self::GracefulShutdown),
            "child_startup_failed" => Ok(Self::ChildStartupFailed),
            "child_restart_exhausted" => Ok(Self::ChildRestartExhausted),
            "heartbeat_expired" => Ok(Self::HeartbeatExpired),
            "logical_node_retired" => Ok(Self::LogicalNodeRetired),
            _ => Err(VoomError::database(format!(
                "{field} {value:?} not in node incarnation end reason vocab"
            ))),
        }
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

    /// Parses a stored value from the worker-kind vocabulary.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when `value` is outside the worker-kind vocabulary.
    /// The error message includes the supplied `field` and `value`.
    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value).ok_or_else(|| {
            VoomError::database(format!("{field} {value:?} not in worker kind vocab"))
        })
    }
}

/// Node-agent proof state for one declared remote worker.
///
/// Activation creates the durable worker identity in [`Self::NotReady`]. The
/// supervising agent transitions it to [`Self::Ready`] only after the child has
/// bound, returned matching accelerator metadata when configured, completed the
/// version handshake, proved its worker identity, and finished dependency
/// preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerReadiness {
    Ready,
    NotReady,
}

/// Durable worker lifecycle state.
///
/// `Registered` remains executable for local and synthetic workers. For remote
/// workers it means declared but not ready; only `Active` may receive new work.
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

    /// Parses a stored value from the worker-status vocabulary.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when `value` is outside the worker-status vocabulary.
    /// The error message includes the supplied `field` and `value`.
    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value).ok_or_else(|| {
            VoomError::database(format!("{field} {value:?} not in worker status vocab"))
        })
    }
}

#[cfg(test)]
#[path = "execution_vocab_test.rs"]
mod tests;
