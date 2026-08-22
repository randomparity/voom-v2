//! Simulated storage-owner node for fenced commit-intent tests (ADR 0074).
//!
//! The commit driver prepares a fenced intent row and waits a bounded time
//! for the storage-owner node to drive it through the authorize / receipt /
//! complete case functions. These helpers stand in for that node:
//! [`SimulatedOwnerNode::install`] flips the seeded test root-owner node into
//! a remote-authenticated principal with an active incarnation, and
//! [`SimulatedOwnerNode::drive_pending_commit`] performs exactly what a real
//! node agent does — authorize, journal, promote the bytes no-replace, report
//! typed evidence, and complete with the fence.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use secrecy::{ExposeSecret, SecretString};
use sqlx::SqlitePool;
use voom_control_plane::artifact_commit::{
    AppliedEvidence, AuthorizeCommitOutcome, CommitOutcomeEvidence, MismatchedEvidence,
    RemoteCommitApplyingInput, RemoteCommitApplyingOutcome, RemoteCommitAuthorizeInput,
    RemoteCommitCompleteInput, RemoteCommitCompleteOutcome, RemoteCommitOutcomeInput,
    RemoteCommitReceiptOutcome,
};
use voom_core::{ArtifactHandleId, NodeId, NodeIncarnationId, VoomError};
use voom_store::repo::media::artifact_commit_intents::CommitObservedFacts;

/// The seeded test storage-root owner, flipped into the simulated remote node
/// by [`SimulatedOwnerNode::install`].
pub const SIMULATED_OWNER_NODE_ID: NodeId = NodeId(9_000_001);

const SIMULATED_TOKEN: &str = "voom-node-v1.simulated-storage-owner-token";
const INSTALL_TIME: &str = "1970-01-01T00:00:00Z";

/// Process-wide counter so every fresh case call carries a unique idempotency
/// key while deliberate replays reuse one key.
static CALL_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_idempotency_key(case: &str) -> String {
    format!(
        "sim-{case}-{}",
        CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// A test double for the storage-owner node agent that owns the target root.
#[derive(Debug, Clone)]
pub struct SimulatedOwnerNode {
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
}

impl SimulatedOwnerNode {
    /// Create the simulated principal with a fresh random incarnation.
    ///
    /// # Errors
    ///
    /// Returns an error when the incarnation id cannot be generated.
    pub fn new() -> Result<Self, VoomError> {
        Ok(Self {
            node_id: SIMULATED_OWNER_NODE_ID,
            token: SecretString::from(SIMULATED_TOKEN.to_owned()),
            incarnation_id: NodeIncarnationId::generate()?,
        })
    }

    /// Flip the seeded root-owner node into a remote node carrying this
    /// principal's token hash and point its active-incarnation fence at the
    /// inserted active incarnation row.
    ///
    /// # Errors
    ///
    /// Returns database errors from the raw fixture writes.
    pub async fn install(&self, pool: &SqlitePool) -> Result<(), VoomError> {
        self.install_for(pool, self.node_id).await
    }

    /// Install this principal on an existing node row (any seeded owner).
    ///
    /// # Errors
    ///
    /// Returns database errors from the raw fixture writes.
    pub async fn install_for(&self, pool: &SqlitePool, node_id: NodeId) -> Result<(), VoomError> {
        let token_hash = voom_control_plane::workers::hash_node_token(self.token.expose_secret());
        let hint = format!("sim-{}", self.node_id.0);
        let node_id_i64 = i64::try_from(node_id.0)
            .map_err(|error| VoomError::database(format!("node id out of range: {error}")))?;
        // One BEGIN IMMEDIATE transaction: the three raw fixture writes must
        // not interleave with concurrent test writers (CLI child processes,
        // worker providers, the driver thread). A deferred transaction would
        // upgrade read-to-write and can surface SQLITE_BUSY under contention
        // (the same class fixed on the audio sidecar commit path).
        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| VoomError::database_context("simulated node install begin", error))?;
        sqlx::query(
            "UPDATE nodes SET kind = 'remote', auth_token_hash = ?, auth_token_hint = ? \
                     WHERE id = ?",
        )
        .bind(&token_hash)
        .bind(&hint)
        .bind(node_id_i64)
        .execute(&mut *tx)
        .await
        .map_err(|error| VoomError::database_context("simulated node install", error))?;
        let incarnation_hex = self.incarnation_id.to_string();
        sqlx::query(
            "INSERT INTO node_incarnations \
                     (incarnation_id, node_id, status, started_at, last_seen_at) \
                     VALUES (?, ?, 'active', ?, ?)",
        )
        .bind(&incarnation_hex)
        .bind(node_id_i64)
        .bind(INSTALL_TIME)
        .bind(INSTALL_TIME)
        .execute(&mut *tx)
        .await
        .map_err(|error| VoomError::database_context("simulated node incarnation", error))?;
        sqlx::query("UPDATE nodes SET active_incarnation_id = ?, status = 'active' WHERE id = ?")
            .bind(&incarnation_hex)
            .bind(node_id_i64)
            .execute(&mut *tx)
            .await
            .map_err(|error| VoomError::database_context("simulated node activation", error))?;
        tx.commit()
            .await
            .map_err(|error| VoomError::database_context("simulated node install commit", error))?;
        Ok(())
    }

    /// Authorize the pending intent (spec step 2).
    ///
    /// # Errors
    ///
    /// Propagates the control-plane case error verbatim.
    pub async fn authorize(
        &self,
        cp: &voom_control_plane::ControlPlane,
        intent_id: voom_core::ids::ArtifactCommitIntentId,
    ) -> Result<AuthorizeCommitOutcome, VoomError> {
        cp.remote_authorize_commit_intent(RemoteCommitAuthorizeInput {
            intent_id,
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            idempotency_key: next_idempotency_key("authorize"),
            request_hash: next_idempotency_key("authorize-hash"),
        })
        .await
    }

    /// Journal the `applying` receipt before touching bytes (spec step 3).
    ///
    /// # Errors
    ///
    /// Propagates the control-plane case error verbatim.
    pub async fn report_applying(
        &self,
        cp: &voom_control_plane::ControlPlane,
        intent_id: voom_core::ids::ArtifactCommitIntentId,
    ) -> Result<RemoteCommitApplyingOutcome, VoomError> {
        cp.remote_report_commit_applying(RemoteCommitApplyingInput {
            intent_id,
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            idempotency_key: next_idempotency_key("applying"),
            request_hash: next_idempotency_key("applying-hash"),
        })
        .await
    }

    /// Report typed outcome evidence (spec step 4/7).
    ///
    /// # Errors
    ///
    /// Propagates the control-plane case error verbatim.
    pub async fn report_outcome(
        &self,
        cp: &voom_control_plane::ControlPlane,
        intent_id: voom_core::ids::ArtifactCommitIntentId,
        evidence: CommitOutcomeEvidence,
    ) -> Result<RemoteCommitReceiptOutcome, VoomError> {
        cp.remote_report_commit_outcome(RemoteCommitOutcomeInput {
            intent_id,
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            idempotency_key: next_idempotency_key("outcome"),
            request_hash: next_idempotency_key("outcome-hash"),
            evidence,
        })
        .await
    }

    /// Complete the authorized intent with the fenced payload (spec step 6).
    ///
    /// # Errors
    ///
    /// Propagates the control-plane case error verbatim.
    pub async fn complete(
        &self,
        cp: &voom_control_plane::ControlPlane,
        intent_id: voom_core::ids::ArtifactCommitIntentId,
        fence_hex: &str,
    ) -> Result<RemoteCommitCompleteOutcome, VoomError> {
        cp.remote_complete_commit_intent(RemoteCommitCompleteInput {
            intent_id,
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            idempotency_key: next_idempotency_key("complete"),
            request_hash: next_idempotency_key("complete-hash"),
            fence_hex: fence_hex.to_owned(),
        })
        .await
    }

    /// Wait for the newest pending intent of the artifact handle, then drive
    /// it to completion: authorize, applying receipt, no-replace promotion of
    /// the staged bytes (an already-promoted matching target counts as
    /// applied), applied evidence, and fenced completion. Drift evidence is
    /// reported as `mismatched` and stops the driver before completion.
    ///
    /// # Errors
    ///
    /// Propagates case errors and gives up as `NotFound` when no pending
    /// intent appears within roughly five seconds.
    pub async fn drive_pending_commit(
        &self,
        cp: &voom_control_plane::ControlPlane,
        pool: &SqlitePool,
        artifact_handle_id: ArtifactHandleId,
    ) -> Result<(), VoomError> {
        let intent_id = wait_for_pending_intent(pool, artifact_handle_id).await?;
        let outcome = self.authorize(cp, intent_id).await?;
        self.report_applying(cp, intent_id).await?;

        let staging_path = resolve_rooted_path(
            pool,
            outcome.staging_storage_root_id.0,
            &outcome.staging_provider_relative_locator,
        )
        .await?;
        let target_path = resolve_rooted_path(
            pool,
            outcome.target_storage_root_id.0,
            &outcome.target_provider_relative_locator,
        )
        .await?;
        let expected = CommitObservedFacts {
            size_bytes: outcome.expected_size_bytes,
            content_hash: outcome.expected_content_hash.clone(),
        };
        let staged_bytes = read_file(&staging_path).await?;
        let staged_facts = observed_facts(&staged_bytes);
        let evidence = if staged_facts == expected {
            match tokio::fs::try_exists(&target_path).await {
                Ok(true) => {
                    let existing = read_file(&target_path).await?;
                    let existing_facts = observed_facts(&existing);
                    if existing_facts == staged_facts {
                        // A prior crashed attempt already promoted matching bytes.
                        CommitOutcomeEvidence::Applied(AppliedEvidence {
                            observed: existing_facts,
                        })
                    } else {
                        CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
                            reason: "target already exists with different bytes".to_owned(),
                            observed: Some(existing_facts),
                        })
                    }
                }
                Ok(false) => {
                    write_new_file(&target_path, &staged_bytes).await?;
                    CommitOutcomeEvidence::Applied(AppliedEvidence {
                        observed: staged_facts,
                    })
                }
                Err(error) => {
                    return Err(VoomError::ArtifactUnavailable(format!(
                        "cannot stat promoted target {}: {error}",
                        target_path.display()
                    )));
                }
            }
        } else {
            CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
                reason: "staged bytes do not match the pinned expected facts".to_owned(),
                observed: Some(staged_facts),
            })
        };
        self.report_outcome(cp, intent_id, evidence.clone()).await?;
        if matches!(evidence, CommitOutcomeEvidence::Mismatched(_)) {
            return Ok(());
        }
        self.complete(cp, intent_id, &outcome.fence_hex).await?;
        Ok(())
    }
}

/// Spawn a background thread that installs the simulated owner on the pool's
/// seeded root and drives every pending commit intent to convergence.
/// Integration-suite stand-in for the storage-owner agent (ADR 0074):
/// non-blocked commits converge while the test observes durable events.
///
/// # Panics
///
/// Panics when the simulated owner cannot be installed or a driver step
/// fails; integration setups fail loudly by design.
#[expect(
    clippy::unwrap_used,
    reason = "the driver runs on a detached thread where Result plumbing cannot \
              surface; a failed setup or driver step must panic the thread"
)]
pub fn install_and_spawn_driver(pool: &SqlitePool) {
    let driver_pool = pool.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let node = SimulatedOwnerNode::new().unwrap();
            node.install(&driver_pool).await.unwrap();
            let driver_cp = voom_control_plane::ControlPlane::open_with_pool(
                driver_pool.clone(),
                std::sync::Arc::new(voom_core::SystemClock),
            )
            .await
            .unwrap();
            loop {
                let pending: Option<(i64, i64)> = sqlx::query_as(
                    "SELECT id, artifact_handle_id FROM artifact_commit_intents \
                     WHERE state = 'pending' ORDER BY id ASC LIMIT 1",
                )
                .fetch_optional(&driver_pool)
                .await
                .unwrap();
                if let Some((_, handle)) = pending {
                    let _ = node
                        .drive_pending_commit(
                            &driver_cp,
                            &driver_pool,
                            ArtifactHandleId(u64::try_from(handle).unwrap()),
                        )
                        .await;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });
    });
}

/// Poll for the newest pending intent of an artifact handle.
///
/// # Errors
///
/// Database errors propagate; a poll timeout becomes `NotFound`.
async fn wait_for_pending_intent(
    pool: &SqlitePool,
    artifact_handle_id: ArtifactHandleId,
) -> Result<voom_core::ids::ArtifactCommitIntentId, VoomError> {
    let handle = i64::try_from(artifact_handle_id.0)
        .map_err(|error| VoomError::database(format!("handle id out of range: {error}")))?;
    for _ in 0..200 {
        let pending: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM artifact_commit_intents \
             WHERE artifact_handle_id = ? AND state = 'pending' ORDER BY id DESC LIMIT 1",
        )
        .bind(handle)
        .fetch_optional(pool)
        .await
        .map_err(|error| VoomError::database_context("pending intent poll", error))?;
        if let Some(id) = pending {
            let id = u64::try_from(id)
                .map_err(|error| VoomError::database(format!("intent id out of range: {error}")))?;
            return Ok(voom_core::ids::ArtifactCommitIntentId(id));
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    Err(VoomError::NotFound(format!(
        "no pending commit intent appeared for artifact handle {}",
        artifact_handle_id.0
    )))
}

async fn resolve_rooted_path(
    pool: &SqlitePool,
    storage_root_id: u64,
    relative_locator: &str,
) -> Result<PathBuf, VoomError> {
    let root_id = i64::try_from(storage_root_id)
        .map_err(|error| VoomError::database(format!("root id out of range: {error}")))?;
    let locator: String =
        sqlx::query_scalar("SELECT provider_locator FROM library_roots WHERE id = ?")
            .bind(root_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| VoomError::database_context("library root lookup", error))?
            .ok_or_else(|| VoomError::NotFound(format!("library_roots {storage_root_id}")))?;
    let root_path = tokio::fs::canonicalize(&locator).await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "cannot resolve storage root {storage_root_id} at {locator}: {error}"
        ))
    })?;
    Ok(root_path.join(relative_locator))
}

async fn read_file(path: &Path) -> Result<Vec<u8>, VoomError> {
    tokio::fs::read(path).await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!("cannot read {}: {error}", path.display()))
    })
}

async fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), VoomError> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|error| {
            VoomError::ArtifactUnavailable(format!(
                "no-replace promotion of {} failed: {error}",
                path.display()
            ))
        })?;
    file.write_all(bytes).await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "writing promoted target {} failed: {error}",
            path.display()
        ))
    })?;
    file.flush().await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "flushing promoted target {} failed: {error}",
            path.display()
        ))
    })?;
    Ok(())
}

/// Facts a node observes on real bytes, matching the pinned-fact encoding.
#[must_use]
pub fn observed_facts(bytes: &[u8]) -> CommitObservedFacts {
    CommitObservedFacts {
        size_bytes: bytes.len() as u64,
        content_hash: format!("blake3:{}", blake3::hash(bytes).to_hex()),
    }
}
