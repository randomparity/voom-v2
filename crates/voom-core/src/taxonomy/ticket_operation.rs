//! Typed operation token used by tickets, worker capabilities, and scheduling.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::error::VoomError;
use crate::operation_kind::OperationKind;

/// The namespace the workflow planner renders its ticket kinds into.
///
/// Reserved: a suffix inside it that no [`OperationKind`] claims is an unknown
/// operation, never a custom local one.
pub const WORKFLOW_OPERATION_NAMESPACE: &str = "synthetic.workflow.operation.";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TicketOperation(String);

impl TicketOperation {
    /// Create a ticket operation token from trusted configuration input.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when the token is empty or contains
    /// characters outside the ticket-operation wire format.
    pub fn new(value: impl Into<String>) -> Result<Self, VoomError> {
        let value = value.into();
        validate_operation_token(&value).map_err(|reason| {
            VoomError::Config(format!("invalid operation {value:?}: {reason}"))
        })?;
        Ok(Self(value))
    }

    /// Rebuild a ticket operation token loaded from persistent storage.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when the stored token is empty or
    /// contains characters outside the ticket-operation wire format.
    pub fn from_stored(value: impl Into<String>, field: &str) -> Result<Self, VoomError> {
        let value = value.into();
        validate_operation_token(&value).map_err(|reason| {
            VoomError::database(format!("{field} invalid operation {value:?}: {reason}"))
        })?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// Classify this token against the operation vocabulary.
    ///
    /// Total and infallible by design. Classification says what a token *is*;
    /// whether that is acceptable depends on the caller — `parse_ticket` accepts
    /// only a namespaced known operation, the lease path rejects an unknown
    /// namespaced one, and the capability lookups tolerate every outcome. A
    /// fallible normalization would push an error into call sites that loop over
    /// candidates and cannot raise without stalling well-formed work.
    #[must_use]
    pub fn normalize(&self) -> NormalizedTicketOperation {
        if let Some(suffix) = self.0.strip_prefix(WORKFLOW_OPERATION_NAMESPACE) {
            return OperationKind::from_wire(suffix).map_or_else(
                || NormalizedTicketOperation::UnknownNamespaced(self.clone()),
                |kind| NormalizedTicketOperation::Known {
                    kind,
                    namespaced: true,
                },
            );
        }
        OperationKind::from_wire(&self.0).map_or_else(
            || NormalizedTicketOperation::CustomLocal(self.clone()),
            |kind| NormalizedTicketOperation::Known {
                kind,
                namespaced: false,
            },
        )
    }
}

/// What [`TicketOperation::normalize`] made of a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedTicketOperation {
    /// A known operation. `namespaced` records whether the token arrived inside
    /// [`WORKFLOW_OPERATION_NAMESPACE`] or as a bare wire token, which is what
    /// lets `parse_ticket` keep accepting only the namespaced encoding.
    Known {
        kind: OperationKind,
        namespaced: bool,
    },
    /// An exact token outside every reserved namespace, such as `disk.test`.
    CustomLocal(TicketOperation),
    /// Inside a reserved namespace, with a suffix no [`OperationKind`] claims —
    /// including an empty one.
    UnknownNamespaced(TicketOperation),
}

impl NormalizedTicketOperation {
    #[must_use]
    pub const fn operation_kind(&self) -> Option<OperationKind> {
        match self {
            Self::Known { kind, .. } => Some(*kind),
            Self::CustomLocal(_) | Self::UnknownNamespaced(_) => None,
        }
    }

    /// The token to match `worker_capabilities.operation` and grant rows against.
    ///
    /// A known operation matches on its bare wire token whichever encoding it
    /// arrived in. Everything else matches on the original token, unmodified:
    /// stripping the namespace off an unrecognized token fabricates a bare name
    /// no row can hold and nothing else can produce.
    #[must_use]
    pub fn matching_token(&self) -> TicketOperation {
        match self {
            Self::Known { kind, .. } => TicketOperation::from(*kind),
            Self::CustomLocal(token) | Self::UnknownNamespaced(token) => token.clone(),
        }
    }
}

impl Display for TicketOperation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<OperationKind> for TicketOperation {
    fn from(value: OperationKind) -> Self {
        Self(value.as_str().to_owned())
    }
}

fn validate_operation_token(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("empty");
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        Ok(())
    } else {
        Err("allowed characters are ASCII letters, digits, '_', '-', and '.'")
    }
}

#[cfg(test)]
#[path = "ticket_operation_test.rs"]
mod tests;
