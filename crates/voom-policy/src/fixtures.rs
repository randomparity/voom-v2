use crate::PolicyInputSetDraft;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixtureName {
    SyntheticCompliantBaseline,
    SyntheticNoncompliantTranscodeNeeded,
}

pub fn load_fixture(name: FixtureName) -> Result<PolicyInputSetDraft, serde_json::Error> {
    serde_json::from_str(match name {
        FixtureName::SyntheticCompliantBaseline => {
            include_str!("../fixtures/synthetic_compliant_baseline.json")
        }
        FixtureName::SyntheticNoncompliantTranscodeNeeded => {
            include_str!("../fixtures/synthetic_noncompliant_transcode_needed.json")
        }
    })
}
