//! In-crate test helpers for driving fenced commit intents (ADR 0074).


use voom_test_support::commit_node::{SimulatedOwnerNode, observed_facts};


use crate::ControlPlane;
use super::tests::{
    intent_state, node_authorize, node_complete, node_report_applying, node_report_outcome,
    rooted_path,
};
use crate::artifact::commit::intent::{
    AppliedEvidence, CommitOutcomeEvidence, MismatchedEvidence,
};
use voom_core::ids::ArtifactCommitIntentId;
use voom_core::VoomError;

pub(crate) fn spawn_auto_driver(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
) -> tokio::task::JoinHandle<()> {
    let cp = cp.clone();
    let node = node.clone();
    tokio::spawn(async move {
        loop {
            let pending: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM artifact_commit_intents WHERE state = 'pending' \
                 ORDER BY id ASC LIMIT 1",
            )
            .fetch_optional(cp.pool_for_test())
            .await
            .unwrap();
            if let Some(id) = pending {
                let intent_id = ArtifactCommitIntentId(u64::try_from(id).unwrap());
                let _ = drive_one(&cp, &node, intent_id).await;
            }
            // Slow poll: the driver only adds read pressure; the driver wait in
            // commit_artifact is seconds-scale.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    })
}

/// Drive one intent through its remaining node steps from whatever state it is
/// in (pending: full sequence; authorized without receipt: journal + promote;
/// authorized with applied receipt: complete).
pub(crate) async fn drive_one(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
    intent_id: ArtifactCommitIntentId,
) -> Result<(), VoomError> {
    if !matches!(intent_state(cp, intent_id).await.as_str(), "pending" | "authorized") {
        return Ok(());
    }
    let outcome = node_authorize(cp, node, intent_id).await?;
    node_report_applying(cp, node, intent_id).await?;
    let staging_path = rooted_path(
        cp,
        outcome.staging_storage_root_id.0,
        &outcome.staging_provider_relative_locator,
    )
    .await;
    let target_path = rooted_path(
        cp,
        outcome.target_storage_root_id.0,
        &outcome.target_provider_relative_locator,
    )
    .await;
    let staged_bytes = std::fs::read(&staging_path).unwrap();
    let staged_facts = observed_facts(&staged_bytes);
    let evidence = if target_path.exists() {
        let existing = std::fs::read(&target_path).unwrap();
        let existing_facts = observed_facts(&existing);
        if existing_facts == staged_facts {
            CommitOutcomeEvidence::Applied(AppliedEvidence { observed: existing_facts })
        } else {
            CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
                reason: "target already exists with different bytes".to_owned(),
                observed: Some(existing_facts),
            })
        }
    } else if staged_facts.size_bytes == outcome.expected_size_bytes
        && staged_facts.content_hash == outcome.expected_content_hash
    {
        std::fs::write(&target_path, &staged_bytes).unwrap();
        CommitOutcomeEvidence::Applied(AppliedEvidence { observed: staged_facts })
    } else {
        CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
            reason: "staged bytes do not match the pinned expected facts".to_owned(),
            observed: Some(staged_facts),
        })
    };
    let is_mismatch = matches!(evidence, CommitOutcomeEvidence::Mismatched(_));
    node_report_outcome(cp, node, intent_id, evidence).await?;
    if is_mismatch {
        return Ok(());
    }
    node_complete(cp, node, intent_id, &outcome.fence_hex).await?;
    Ok(())
}
