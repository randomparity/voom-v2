use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("failed to read scenario {path:?}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to decode scenario {path:?}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// One scripted event a fake's operation handler consumes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioEvent {
    DiscoverFile {
        path: String,
        size: u64,
    },
    ScanComplete {
        duration_ms: u32,
    },
    Custom {
        name: String,
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub scenario: String,
    pub events: Vec<ScenarioEvent>,
}

pub fn load_scenario(path: impl AsRef<Path>) -> Result<Scenario, ScenarioError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|source| ScenarioError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| ScenarioError::Decode {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, Clone)]
pub struct ScenarioPlayer {
    events: std::vec::IntoIter<ScenarioEvent>,
}

impl ScenarioPlayer {
    #[must_use]
    pub fn new(scenario: Scenario) -> Self {
        Self {
            events: scenario.events.into_iter(),
        }
    }

    pub fn next_event(&mut self) -> Option<ScenarioEvent> {
        self.events.next()
    }
}

#[cfg(test)]
#[path = "scenario_test.rs"]
mod tests;
