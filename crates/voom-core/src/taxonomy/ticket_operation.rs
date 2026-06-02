//! Typed operation token used by tickets, worker capabilities, and scheduling.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::error::VoomError;
use crate::operation_kind::OperationKind;

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
            VoomError::Database(format!("{field} invalid operation {value:?}: {reason}"))
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
