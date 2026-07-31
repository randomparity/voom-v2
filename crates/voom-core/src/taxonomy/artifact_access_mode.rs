use crate::VoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAccessMode {
    SharedMount,
    ControlPlanePlaceholder,
    StagedOutputPlaceholder,
}

impl ArtifactAccessMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SharedMount => "shared_mount",
            Self::ControlPlanePlaceholder => "control_plane_placeholder",
            Self::StagedOutputPlaceholder => "staged_output_placeholder",
        }
    }

    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "shared_mount" => Some(Self::SharedMount),
            "control_plane_placeholder" => Some(Self::ControlPlanePlaceholder),
            "staged_output_placeholder" => Some(Self::StagedOutputPlaceholder),
            _ => None,
        }
    }

    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        Self::from_wire(value).ok_or_else(|| {
            VoomError::database(format!(
                "{field} {value:?} not in artifact access mode vocab"
            ))
        })
    }
}

#[cfg(test)]
#[path = "artifact_access_mode_test.rs"]
mod tests;
