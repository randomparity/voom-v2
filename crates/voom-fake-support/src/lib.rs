#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Shared helpers for Sprint 2 fake-provider binaries.
//!
//! Consumed only by the eleven `fake-*` binaries in `voom-fakes`.
//! `chaos-worker`, `benchmark-worker`, and `voom-conformance` do
//! NOT depend on this crate — keeping their behavior independent
//! of any shared encoder/decoder bug.

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("read: {0}")]
    Read(String),
    #[error("decode: {0}")]
    Decode(String),
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
    let bytes = std::fs::read(path.as_ref()).map_err(|e| ScenarioError::Read(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| ScenarioError::Decode(e.to_string()))
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
#[path = "lib_test.rs"]
mod tests;
