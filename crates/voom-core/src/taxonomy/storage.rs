use serde::{Deserialize, Deserializer, Serialize};

use crate::VoomError;

const MAX_LOCATOR_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageProviderKind {
    LocalFilesystem,
}

impl StorageProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalFilesystem => "local_filesystem",
        }
    }

    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "local_filesystem" => Some(Self::LocalFilesystem),
            _ => None,
        }
    }

    /// Parse a provider kind read from durable storage.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when `value` is outside the provider vocabulary.
    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value).ok_or_else(|| {
            VoomError::database(format!(
                "{field} {value:?} not in storage provider kind vocab"
            ))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageRootState {
    Unassigned,
    Configured,
    Active,
    Unavailable,
    Retired,
}

impl StorageRootState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unassigned => "unassigned",
            Self::Configured => "configured",
            Self::Active => "active",
            Self::Unavailable => "unavailable",
            Self::Retired => "retired",
        }
    }

    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "unassigned" => Some(Self::Unassigned),
            "configured" => Some(Self::Configured),
            "active" => Some(Self::Active),
            "unavailable" => Some(Self::Unavailable),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }

    /// Parse a root state read from durable storage.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when `value` is outside the state vocabulary.
    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value).ok_or_else(|| {
            VoomError::database(format!("{field} {value:?} not in storage root state vocab"))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProviderLocator(String);

impl ProviderLocator {
    /// Construct an opaque provider locator after applying transport-safe bounds.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when the locator is empty, contains NUL, or is too long.
    pub fn new(value: String) -> Result<Self, VoomError> {
        validate_provider_locator(&value)
            .map_err(|reason| VoomError::Config(format!("provider locator invalid: {reason}")))?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    /// Parse an opaque provider locator read from durable storage.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when the persisted locator violates its bounds.
    pub fn parse_database(field: &str, value: String) -> Result<Self, VoomError> {
        validate_provider_locator(&value).map_err(|reason| {
            VoomError::database(format!(
                "{field} contains invalid provider locator: {reason}"
            ))
        })?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for ProviderLocator {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProviderRelativeLocator(String);

impl ProviderRelativeLocator {
    /// Construct a provider-relative locator that cannot syntactically escape its root.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when the locator is absolute, ambiguous, or unbounded.
    pub fn new(value: String) -> Result<Self, VoomError> {
        validate_relative_locator(&value).map_err(|reason| {
            VoomError::Config(format!("provider-relative locator invalid: {reason}"))
        })?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    /// Parse a provider-relative locator read from durable storage.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Database`] when persisted data violates locator invariants.
    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        validate_relative_locator(value).map_err(|reason| {
            VoomError::database(format!(
                "{field} contains invalid provider-relative locator: {reason}"
            ))
        })?;
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for ProviderRelativeLocator {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

fn validate_provider_locator(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }
    if value.len() > MAX_LOCATOR_BYTES {
        return Err("must be at most 4096 UTF-8 bytes");
    }
    if value.contains('\0') {
        return Err("must not contain NUL");
    }
    Ok(())
}

fn validate_relative_locator(value: &str) -> Result<(), &'static str> {
    validate_provider_locator(value)?;
    if value.starts_with('/') || value.ends_with('/') {
        return Err("must not be absolute or end with a separator");
    }
    if value.contains('\\') {
        return Err("must use forward-slash separators");
    }
    let mut components = value.split('/');
    let first = components.next().ok_or("must not be empty")?;
    if is_windows_drive_prefix(first) {
        return Err("must not contain a platform path prefix");
    }
    if invalid_component(first) || components.any(invalid_component) {
        return Err("components must not be empty, '.' or '..'");
    }
    Ok(())
}

fn invalid_component(component: &str) -> bool {
    component.is_empty() || component == "." || component == ".."
}

fn is_windows_drive_prefix(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
#[path = "storage_test.rs"]
mod tests;
