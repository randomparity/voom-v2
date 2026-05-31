use std::{error::Error, fmt, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyFixture {
    pub source_path: &'static str,
    pub expected_json_path: &'static str,
}

#[derive(Debug)]
pub enum PolicyFixtureError {
    Io {
        path: String,
        source: std::io::Error,
    },
    Json {
        path: String,
        source: serde_json::Error,
    },
    UnknownPath(String),
}

impl fmt::Display for PolicyFixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{path}: {source}"),
            Self::Json { path, source } => write!(f, "{path}: {source}"),
            Self::UnknownPath(path) => write!(f, "unknown policy fixture path: {path}"),
        }
    }
}

impl Error for PolicyFixtureError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::UnknownPath(_) => None,
        }
    }
}

const VALID_POLICY_FIXTURES: &[PolicyFixture] = &[
    PolicyFixture {
        source_path: "fixtures/policies/minimal.voom",
        expected_json_path: "fixtures/compiled/minimal.json",
    },
    PolicyFixture {
        source_path: "fixtures/policies/container-metadata.voom",
        expected_json_path: "fixtures/compiled/container-metadata.json",
    },
    PolicyFixture {
        source_path: "fixtures/policies/production-normalize-reduced.voom",
        expected_json_path: "fixtures/compiled/production-normalize-reduced.json",
    },
    PolicyFixture {
        source_path: "fixtures/policies/video-transcode-hevc.voom",
        expected_json_path: "fixtures/compiled/video-transcode-hevc.json",
    },
    PolicyFixture {
        source_path: "fixtures/policies/audio-transcode-extract.voom",
        expected_json_path: "fixtures/compiled/audio-transcode-extract.json",
    },
];

const INVALID_POLICY_FIXTURES: &[PolicyFixture] = &[
    PolicyFixture {
        source_path: "fixtures/policies/invalid-deferred-transcode.voom",
        expected_json_path: "fixtures/diagnostics/invalid-deferred-transcode.json",
    },
    PolicyFixture {
        source_path: "fixtures/policies/invalid-extends.voom",
        expected_json_path: "fixtures/diagnostics/invalid-extends.json",
    },
    PolicyFixture {
        source_path: "fixtures/policies/invalid-extend-phase.voom",
        expected_json_path: "fixtures/diagnostics/invalid-extend-phase.json",
    },
    PolicyFixture {
        source_path: "fixtures/policies/invalid-unknown-core-field.voom",
        expected_json_path: "fixtures/diagnostics/invalid-unknown-core-field.json",
    },
];

#[must_use]
pub const fn valid_policy_fixtures() -> &'static [PolicyFixture] {
    VALID_POLICY_FIXTURES
}

#[must_use]
pub const fn invalid_policy_fixtures() -> &'static [PolicyFixture] {
    INVALID_POLICY_FIXTURES
}

pub fn load_policy_fixture(path: &str) -> Result<String, PolicyFixtureError> {
    if !VALID_POLICY_FIXTURES
        .iter()
        .chain(INVALID_POLICY_FIXTURES)
        .any(|fixture| fixture.source_path == path)
    {
        return Err(PolicyFixtureError::UnknownPath(path.to_owned()));
    }
    read_fixture(path)
}

pub fn load_json_fixture(path: &str) -> Result<serde_json::Value, PolicyFixtureError> {
    let source = read_fixture(path)?;
    serde_json::from_str(&source).map_err(|source| PolicyFixtureError::Json {
        path: path.to_owned(),
        source,
    })
}

fn read_fixture(path: &str) -> Result<String, PolicyFixtureError> {
    let full_path = fixture_path(path);
    std::fs::read_to_string(&full_path).map_err(|source| PolicyFixtureError::Io {
        path: path.to_owned(),
        source,
    })
}

fn fixture_path(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

#[cfg(test)]
#[path = "policy_fixtures_test.rs"]
mod tests;
