use crate::PolicyInputSetDraft;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixtureName {
    SyntheticCompliantBaseline,
    SyntheticNoncompliantTranscodeNeeded,
}

impl FixtureName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SyntheticCompliantBaseline => "synthetic_compliant_baseline",
            Self::SyntheticNoncompliantTranscodeNeeded => "synthetic_noncompliant_transcode_needed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownFixtureName;

impl std::str::FromStr for FixtureName {
    type Err = UnknownFixtureName;

    fn from_str(label: &str) -> Result<Self, Self::Err> {
        match label {
            "synthetic_compliant_baseline" => Ok(Self::SyntheticCompliantBaseline),
            "synthetic_noncompliant_transcode_needed" => {
                Ok(Self::SyntheticNoncompliantTranscodeNeeded)
            }
            _ => Err(UnknownFixtureName),
        }
    }
}

pub fn load_fixture(name: FixtureName) -> Result<PolicyInputSetDraft, serde_json::Error> {
    serde_json::from_str(match name {
        FixtureName::SyntheticCompliantBaseline => {
            include_str!("../../fixtures/synthetic_compliant_baseline.json")
        }
        FixtureName::SyntheticNoncompliantTranscodeNeeded => {
            include_str!("../../fixtures/synthetic_noncompliant_transcode_needed.json")
        }
    })
}

#[cfg(test)]
#[path = "fixtures_test.rs"]
mod tests;
