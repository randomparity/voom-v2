use serde::{Deserialize, Serialize};
use voom_core::{IssueId, PolicyVersionId};

// --- issues ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueLifecyclePayload {
    pub issue_id: IssueId,
    pub kind: String,
    pub status: String,
    pub dedupe_key: Option<String>,
    pub policy_version_id: Option<PolicyVersionId>,
    pub report_id: Option<String>,
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod tests;
