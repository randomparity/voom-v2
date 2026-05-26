#[derive(Debug)]
pub enum GoldenPlanFixtureError {
    UnknownFixture(String),
    Json(serde_json::Error),
}

impl std::fmt::Display for GoldenPlanFixtureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFixture(name) => write!(f, "unknown golden plan fixture: {name}"),
            Self::Json(err) => write!(f, "golden plan fixture JSON parse: {err}"),
        }
    }
}

impl std::error::Error for GoldenPlanFixtureError {}

impl From<serde_json::Error> for GoldenPlanFixtureError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

pub fn load_golden_plan(name: &str) -> Result<serde_json::Value, GoldenPlanFixtureError> {
    let source = match name {
        "container_metadata_compliant" => {
            include_str!("../fixtures/plans/container_metadata_compliant.json")
        }
        "container_metadata_noncompliant" => {
            include_str!("../fixtures/plans/container_metadata_noncompliant.json")
        }
        "remux_track_selection" => include_str!("../fixtures/plans/remux_track_selection.json"),
        _ => return Err(GoldenPlanFixtureError::UnknownFixture(name.to_owned())),
    };
    serde_json::from_str(source).map_err(GoldenPlanFixtureError::from)
}

pub fn load_golden_compliance_report(
    name: &str,
) -> Result<serde_json::Value, GoldenPlanFixtureError> {
    let source = match name {
        "container_metadata_compliant" => {
            include_str!("../fixtures/reports/container_metadata_compliant.json")
        }
        "container_metadata_noncompliant" => {
            include_str!("../fixtures/reports/container_metadata_noncompliant.json")
        }
        "container_metadata_blocked" => {
            include_str!("../fixtures/reports/container_metadata_blocked.json")
        }
        "container_metadata_mixed" => {
            include_str!("../fixtures/reports/container_metadata_mixed.json")
        }
        "remux_track_selection" => {
            include_str!("../fixtures/reports/remux_track_selection.json")
        }
        _ => return Err(GoldenPlanFixtureError::UnknownFixture(name.to_owned())),
    };
    serde_json::from_str(source).map_err(GoldenPlanFixtureError::from)
}

#[cfg(test)]
#[path = "fixtures_test.rs"]
mod tests;
