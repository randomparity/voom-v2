//! Durable repository for `artifact_commit_intents` (migration 0038, ADR
//! 0074): one fenced authorization state machine per staged artifact
//! commit, 1:1 with its `artifact_commit_records` row. The control plane
//! creates pending intents at prepare, authorizes them for the storage
//! owner, journals node receipts, and consumes the one-time commit fence
//! at completion; the node discovers its work through
//! [`SqliteArtifactCommitIntentRepo::list_open_for_roots_in_tx`].

use rand::TryRngCore;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
};
use super::use_leases::LeaseScope;
use crate::tx::begin_read_only;
use voom_core::ids::{
    ArtifactCommitIntentId, ArtifactCommitRecordId, ArtifactHandleId, ArtifactVerificationId,
    FileLocationId, FileVersionId,
};
use voom_core::{NodeId, NodeIncarnationId, ProviderRelativeLocator, StorageRootId, VoomError};

/// Facts a staged commit's bytes must match, pinned at prepare from the
/// verified staging verification. Durable JSON column `expected_facts`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitExpectedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
}

/// Facts a node observed on real bytes, reported back in receipts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitObservedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
}

/// Wire structs for [`CommitReceipt`]. The enum is internally tagged, and
/// serde cannot enforce `deny_unknown_fields` on the tag itself, so each
/// variant carries a dedicated wire struct that rejects unknown fields
/// (same pattern as `commit_safety_gate::codecs`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyingReceipt {
    pub reported_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedReceipt {
    pub observed: CommitObservedFacts,
    pub reported_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MismatchedReceipt {
    pub reason: String,
    pub observed: Option<CommitObservedFacts>,
    pub reported_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeUnknownReceipt {
    pub reason: String,
    pub reported_at: String,
}

/// A node-reported receipt on an authorized intent. `applying` is the
/// mutation gate (the node mutates only after it is durably recorded);
/// receipt absence means not started.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitReceipt {
    Applying(ApplyingReceipt),
    Applied(AppliedReceipt),
    Mismatched(MismatchedReceipt),
    OutcomeUnknown(OutcomeUnknownReceipt),
}

impl CommitReceipt {
    /// Wire vocabulary for the receipt kind, mirroring the serde tag.
    #[must_use]
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::Applying(_) => "applying",
            Self::Applied(_) => "applied",
            Self::Mismatched(_) => "mismatched",
            Self::OutcomeUnknown(_) => "outcome_unknown",
        }
    }
}

/// Lifecycle of one fenced commit intent (ADR 0074): `pending ->
/// authorized | aborted`, `authorized -> completed | recovery_required`,
/// `recovery_required -> completed | aborted`. Terminal states never
/// reopen; a retry prepares a successor generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactCommitIntentState {
    Pending,
    Authorized,
    Completed,
    Aborted,
    RecoveryRequired,
}

impl ArtifactCommitIntentState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Authorized => "authorized",
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::RecoveryRequired => "recovery_required",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "pending" => Ok(Self::Pending),
            "authorized" => Ok(Self::Authorized),
            "completed" => Ok(Self::Completed),
            "aborted" => Ok(Self::Aborted),
            "recovery_required" => Ok(Self::RecoveryRequired),
            other => Err(VoomError::database(format!(
                "artifact_commit_intents.state {other:?} not in vocab"
            ))),
        }
    }
}

/// Input for [`SqliteArtifactCommitIntentRepo::create_pending_in_tx`].
#[derive(Debug, Clone)]
pub struct NewArtifactCommitIntent {
    pub commit_record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub source_file_version_id: FileVersionId,
    pub verification_id: ArtifactVerificationId,
    pub staging_location_id: FileLocationId,
    pub staging_location_epoch: u64,
    pub target_storage_root_id: StorageRootId,
    pub target_root_epoch: u64,
    pub target_provider_relative_locator: String,
    pub owner_node_id: NodeId,
    pub expected_facts: CommitExpectedFacts,
    pub requested_at: OffsetDateTime,
    /// Where the staged bytes come from: the source file version's live
    /// rooted address pinned at prepare (ADR 0075). The node materializes
    /// staging from this handle during `applying`.
    pub source_storage_root_id: StorageRootId,
    pub source_provider_relative_locator: ProviderRelativeLocator,
}

/// One durable fenced commit intent.
#[derive(Clone)]
pub struct ArtifactCommitIntent {
    pub id: ArtifactCommitIntentId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub source_file_version_id: FileVersionId,
    pub verification_id: ArtifactVerificationId,
    pub staging_location_id: FileLocationId,
    pub staging_location_epoch: u64,
    pub target_storage_root_id: StorageRootId,
    pub target_root_epoch: u64,
    pub target_provider_relative_locator: String,
    pub source_storage_root_id: StorageRootId,
    pub source_provider_relative_locator: ProviderRelativeLocator,
    pub owner_node_id: NodeId,
    pub owner_incarnation_id: Option<NodeIncarnationId>,
    pub expected_facts: CommitExpectedFacts,
    pub state: ArtifactCommitIntentState,
    pub intent_epoch: u64,
    pub commit_fence: Option<Vec<u8>>,
    pub receipt: Option<CommitReceipt>,
    pub supplemental_receipt: Option<CommitReceipt>,
    pub requested_at: OffsetDateTime,
    pub authorized_at: Option<OffsetDateTime>,
    pub terminal_at: Option<OffsetDateTime>,
}

impl std::fmt::Debug for ArtifactCommitIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The raw fence is capability material: its Debug rendering must
        // never leak it into a log or telemetry surface.
        f.debug_struct("ArtifactCommitIntent")
            .field("id", &self.id)
            .field("commit_record_id", &self.commit_record_id)
            .field("artifact_handle_id", &self.artifact_handle_id)
            .field("source_file_version_id", &self.source_file_version_id)
            .field("source_storage_root_id", &self.source_storage_root_id)
            .field(
                "source_provider_relative_locator",
                &self.source_provider_relative_locator,
            )
            .field("verification_id", &self.verification_id)
            .field("staging_location_id", &self.staging_location_id)
            .field("staging_location_epoch", &self.staging_location_epoch)
            .field("target_storage_root_id", &self.target_storage_root_id)
            .field("target_root_epoch", &self.target_root_epoch)
            .field(
                "target_provider_relative_locator",
                &self.target_provider_relative_locator,
            )
            .field("owner_node_id", &self.owner_node_id)
            .field("owner_incarnation_id", &self.owner_incarnation_id)
            .field("expected_facts", &self.expected_facts)
            .field("state", &self.state)
            .field("intent_epoch", &self.intent_epoch)
            .field("commit_fence", &"[REDACTED]")
            .field("receipt", &self.receipt)
            .field("supplemental_receipt", &self.supplemental_receipt)
            .field("requested_at", &self.requested_at)
            .field("authorized_at", &self.authorized_at)
            .field("terminal_at", &self.terminal_at)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct SqliteArtifactCommitIntentRepo {
    pool: SqlitePool,
}

impl SqliteArtifactCommitIntentRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteArtifactCommitIntentRepo {}
const SELECT_ARTIFACT_COMMIT_INTENT_COLS: &str = "SELECT i.id, i.commit_record_id, \
    i.artifact_handle_id, i.source_file_version_id, i.verification_id, \
    i.staging_location_id, i.staging_location_epoch, i.source_storage_root_id, \
    i.source_provider_relative_locator, i.target_storage_root_id, \
    i.target_root_epoch, i.target_provider_relative_locator, i.owner_node_id, \
    i.owner_incarnation_id, i.expected_facts, i.state, i.intent_epoch, i.commit_fence, \
    i.receipt, i.supplemental_receipt, i.requested_at, i.authorized_at, i.terminal_at \
    FROM artifact_commit_intents i";

fn checked_id(value: u64, field: &str) -> Result<i64, VoomError> {
    i64_from_u64(value, format!("artifact_commit_intents.{field}"))
}

fn map_intent_insert_err(err: &sqlx::Error) -> VoomError {
    if is_unique_violation(err) {
        VoomError::Conflict(
            "artifact_commit_intents: commit record already has an intent".to_owned(),
        )
    } else {
        VoomError::database(format!("artifact_commit_intents insert: {err}"))
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

fn decode_receipt_column(
    raw: Option<String>,
    column: &str,
) -> Result<Option<CommitReceipt>, VoomError> {
    raw.map(|raw| serde_json::from_str(&raw).map_err(|e| VoomError::database_context(column, e)))
        .transpose()
}

fn encode_receipt(receipt: &CommitReceipt) -> Result<String, VoomError> {
    serialize_json(receipt, "artifact_commit_intents.receipt")
}

fn changed_intent(
    rows_affected: u64,
    id: ArtifactCommitIntentId,
    operation: &str,
) -> Result<(), VoomError> {
    if rows_affected != 1 {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_intents {operation}: id={id} did not match the expected state"
        )));
    }
    Ok(())
}

fn get_col<T>(row: &sqlx::sqlite::SqliteRow, name: &'static str) -> Result<T, VoomError>
where
    T: for<'a> sqlx::decode::Decode<'a, sqlx::Sqlite> + sqlx::types::Type<sqlx::Sqlite>,
{
    row.try_get(name)
        .map_err(|e| map_row_err("artifact_commit_intents", e))
}

fn row_to_intent(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactCommitIntent, VoomError> {
    let id: i64 = get_col(row, "id")?;
    let commit_record_id: i64 = get_col(row, "commit_record_id")?;
    let artifact_handle_id: i64 = get_col(row, "artifact_handle_id")?;
    let source_file_version_id: i64 = get_col(row, "source_file_version_id")?;
    let verification_id: i64 = get_col(row, "verification_id")?;
    let staging_location_id: i64 = get_col(row, "staging_location_id")?;
    let staging_location_epoch: i64 = get_col(row, "staging_location_epoch")?;
    let source_storage_root_id: i64 = get_col(row, "source_storage_root_id")?;
    let source_provider_relative_locator: String =
        get_col(row, "source_provider_relative_locator")?;
    let target_storage_root_id: i64 = get_col(row, "target_storage_root_id")?;
    let target_root_epoch: i64 = get_col(row, "target_root_epoch")?;
    let target_provider_relative_locator: String =
        get_col(row, "target_provider_relative_locator")?;
    let owner_node_id: i64 = get_col(row, "owner_node_id")?;
    let owner_incarnation_id: Option<String> = get_col(row, "owner_incarnation_id")?;
    let expected_facts: String = get_col(row, "expected_facts")?;
    let state: String = get_col(row, "state")?;
    let intent_epoch: i64 = get_col(row, "intent_epoch")?;
    let commit_fence: Option<Vec<u8>> = get_col(row, "commit_fence")?;
    let receipt: Option<String> = get_col(row, "receipt")?;
    let supplemental_receipt: Option<String> = get_col(row, "supplemental_receipt")?;
    let requested_at: String = get_col(row, "requested_at")?;
    let authorized_at: Option<String> = get_col(row, "authorized_at")?;
    let terminal_at: Option<String> = get_col(row, "terminal_at")?;

    Ok(ArtifactCommitIntent {
        id: ArtifactCommitIntentId(u64_from_i64(id, "artifact_commit_intents.id")?),
        commit_record_id: ArtifactCommitRecordId(u64_from_i64(
            commit_record_id,
            "artifact_commit_intents.commit_record_id",
        )?),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(
            artifact_handle_id,
            "artifact_commit_intents.artifact_handle_id",
        )?),
        source_file_version_id: FileVersionId(u64_from_i64(
            source_file_version_id,
            "artifact_commit_intents.source_file_version_id",
        )?),
        verification_id: ArtifactVerificationId(u64_from_i64(
            verification_id,
            "artifact_commit_intents.verification_id",
        )?),
        staging_location_id: FileLocationId(u64_from_i64(
            staging_location_id,
            "artifact_commit_intents.staging_location_id",
        )?),
        staging_location_epoch: u64_from_i64(
            staging_location_epoch,
            "artifact_commit_intents.staging_location_epoch",
        )?,
        target_storage_root_id: StorageRootId(u64_from_i64(
            target_storage_root_id,
            "artifact_commit_intents.target_storage_root_id",
        )?),
        target_root_epoch: u64_from_i64(
            target_root_epoch,
            "artifact_commit_intents.target_root_epoch",
        )?,
        source_storage_root_id: StorageRootId(u64_from_i64(
            source_storage_root_id,
            "artifact_commit_intents.source_storage_root_id",
        )?),
        source_provider_relative_locator: ProviderRelativeLocator::parse_database(
            "artifact_commit_intents.source_provider_relative_locator",
            &source_provider_relative_locator,
        )?,
        target_provider_relative_locator,
        owner_node_id: NodeId(u64_from_i64(
            owner_node_id,
            "artifact_commit_intents.owner_node_id",
        )?),
        owner_incarnation_id: owner_incarnation_id
            .as_deref()
            .map(|value| {
                NodeIncarnationId::parse_database(
                    "artifact_commit_intents.owner_incarnation_id",
                    value,
                )
            })
            .transpose()?,
        expected_facts: serde_json::from_str(&expected_facts).map_err(|e| {
            VoomError::database_context("artifact_commit_intents.expected_facts decode", e)
        })?,
        state: ArtifactCommitIntentState::parse(&state)?,
        intent_epoch: u64_from_i64(intent_epoch, "artifact_commit_intents.intent_epoch")?,
        commit_fence,
        receipt: decode_receipt_column(receipt, "artifact_commit_intents.receipt decode")?,
        supplemental_receipt: decode_receipt_column(
            supplemental_receipt,
            "artifact_commit_intents.supplemental_receipt decode",
        )?,
        requested_at: parse_iso8601(&requested_at)?,
        authorized_at: authorized_at.map(|s| parse_iso8601(&s)).transpose()?,
        terminal_at: terminal_at.map(|s| parse_iso8601(&s)).transpose()?,
    })
}

/// Mint the one-time 32-byte commit fence from the operating-system RNG.
fn mint_commit_fence() -> Result<Vec<u8>, VoomError> {
    let mut fence = [0_u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut fence)
        .map_err(|error| {
            VoomError::Internal(format!("generate commit fence from OS RNG: {error}"))
        })?;
    Ok(fence.to_vec())
}

impl SqliteArtifactCommitIntentRepo {
    pub async fn create_pending_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewArtifactCommitIntent,
    ) -> Result<ArtifactCommitIntent, VoomError> {
        let expected_facts = serialize_json(
            &input.expected_facts,
            "artifact_commit_intents.expected_facts",
        )?;
        let requested_at = iso8601(input.requested_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_commit_intents \
             (commit_record_id, artifact_handle_id, source_file_version_id, verification_id, \
              staging_location_id, staging_location_epoch, source_storage_root_id, \
              source_provider_relative_locator, target_storage_root_id, \
              target_root_epoch, target_provider_relative_locator, owner_node_id, \
              expected_facts, state, intent_epoch, requested_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?)",
        )
        .bind(checked_id(input.commit_record_id.0, "commit_record_id")?)
        .bind(checked_id(
            input.artifact_handle_id.0,
            "artifact_handle_id",
        )?)
        .bind(checked_id(
            input.source_file_version_id.0,
            "source_file_version_id",
        )?)
        .bind(checked_id(input.verification_id.0, "verification_id")?)
        .bind(checked_id(
            input.staging_location_id.0,
            "staging_location_id",
        )?)
        .bind(checked_id(
            input.staging_location_epoch,
            "staging_location_epoch",
        )?)
        .bind(checked_id(
            input.source_storage_root_id.0,
            "source_storage_root_id",
        )?)
        .bind(input.source_provider_relative_locator.as_str())
        .bind(checked_id(
            input.target_storage_root_id.0,
            "target_storage_root_id",
        )?)
        .bind(checked_id(input.target_root_epoch, "target_root_epoch")?)
        .bind(&input.target_provider_relative_locator)
        .bind(checked_id(input.owner_node_id.0, "owner_node_id")?)
        .bind(expected_facts)
        .bind(&requested_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| map_intent_insert_err(&e))?;

        let id = ArtifactCommitIntentId(u64_from_i64(
            res.last_insert_rowid(),
            "artifact_commit_intents.last_insert_rowid",
        )?);
        Ok(ArtifactCommitIntent {
            id,
            commit_record_id: input.commit_record_id,
            artifact_handle_id: input.artifact_handle_id,
            source_file_version_id: input.source_file_version_id,
            verification_id: input.verification_id,
            staging_location_id: input.staging_location_id,
            staging_location_epoch: input.staging_location_epoch,
            source_storage_root_id: input.source_storage_root_id,
            source_provider_relative_locator: input.source_provider_relative_locator,
            target_storage_root_id: input.target_storage_root_id,
            target_root_epoch: input.target_root_epoch,
            target_provider_relative_locator: input.target_provider_relative_locator,
            owner_node_id: input.owner_node_id,
            owner_incarnation_id: None,
            expected_facts: input.expected_facts,
            state: ArtifactCommitIntentState::Pending,
            intent_epoch: 0,
            commit_fence: None,
            receipt: None,
            supplemental_receipt: None,
            requested_at: input.requested_at,
            authorized_at: None,
            terminal_at: None,
        })
    }

    pub async fn require_intent_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitIntentId,
    ) -> Result<ArtifactCommitIntent, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_INTENT_COLS.to_owned() + " WHERE i.id = ?";
        let row = sqlx::query(&sql)
            .bind(checked_id(id.0, "id")?)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("artifact_commit_intents get", e))?
            .ok_or_else(|| VoomError::NotFound(format!("artifact_commit_intent {id} not found")))?;
        row_to_intent(&row)
    }

    /// Pool-level read of one intent for drivers between transactions;
    /// case functions use the `_in_tx` variants inside their own tx.
    pub async fn require_intent(
        &self,
        id: ArtifactCommitIntentId,
    ) -> Result<ArtifactCommitIntent, VoomError> {
        let mut tx = begin_read_only(&self.pool, "artifact_commit_intents: require_intent").await?;
        self.require_intent_in_tx(&mut tx, id).await
    }

    /// Pool-level discovery read for drivers between transactions: the ids
    /// of intents still awaiting authorization, oldest first.
    pub async fn list_pending_intent_ids(
        &self,
        limit: u64,
    ) -> Result<Vec<ArtifactCommitIntentId>, VoomError> {
        let rows: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM artifact_commit_intents \
             WHERE state = 'pending' ORDER BY id ASC LIMIT ?",
        )
        .bind(
            i64::try_from(limit).map_err(|_| {
                VoomError::database("artifact_commit_intents list limit out of range")
            })?,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_intents pending listing", e))?;
        rows.iter()
            .map(|raw| u64_from_i64(*raw, "artifact_commit_intents.id").map(ArtifactCommitIntentId))
            .collect()
    }

    pub async fn get_by_commit_record_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        commit_record_id: ArtifactCommitRecordId,
    ) -> Result<Option<ArtifactCommitIntent>, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_INTENT_COLS.to_owned() + " WHERE i.commit_record_id = ?";
        let row = sqlx::query(&sql)
            .bind(checked_id(commit_record_id.0, "commit_record_id")?)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| {
                VoomError::database_context("artifact_commit_intents get by commit record", e)
            })?;
        row.as_ref().map(row_to_intent).transpose()
    }

    /// Transition `pending -> authorized` (CAS on `intent_epoch`): mint and
    /// store the one-time 32-byte commit fence, record the owner
    /// incarnation, and bump the epoch. Drift (wrong state or concurrent
    /// epoch) fails with `Conflict` — the caller aborts fail-closed.
    pub async fn authorize_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitIntentId,
        owner_incarnation_id: NodeIncarnationId,
        now: OffsetDateTime,
    ) -> Result<ArtifactCommitIntent, VoomError> {
        let current = self.require_intent_in_tx(tx, id).await?;
        if current.state != ArtifactCommitIntentState::Pending {
            return Err(VoomError::Conflict(format!(
                "artifact_commit_intents authorize: id={id} is {} not pending",
                current.state.as_str()
            )));
        }
        let fence = mint_commit_fence()?;
        let authorized_at = iso8601(now)?;
        let incarnation = owner_incarnation_id.to_string();
        let res = sqlx::query(
            "UPDATE artifact_commit_intents \
             SET state = 'authorized', commit_fence = ?, authorized_at = ?, \
                 owner_incarnation_id = ?, intent_epoch = intent_epoch + 1 \
             WHERE id = ? AND state = 'pending' AND intent_epoch = ?",
        )
        .bind(&fence)
        .bind(&authorized_at)
        .bind(&incarnation)
        .bind(checked_id(id.0, "id")?)
        .bind(checked_id(current.intent_epoch, "intent_epoch")?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_intents authorize", e))?;
        changed_intent(res.rows_affected(), id, "authorize")?;
        let mut authorized = self.require_intent_in_tx(tx, id).await?;
        authorized.commit_fence = Some(fence);
        Ok(authorized)
    }

    /// Journal a node receipt on an authorized intent (CAS on
    /// `intent_epoch`). `applying` is the mutation gate and may overwrite
    /// only an absent receipt; every other receipt may follow only an
    /// `applying` receipt. Ordering violations fail with `Conflict`.
    pub async fn record_receipt_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitIntentId,
        receipt: CommitReceipt,
    ) -> Result<ArtifactCommitIntent, VoomError> {
        let current = self.require_intent_in_tx(tx, id).await?;
        if current.state != ArtifactCommitIntentState::Authorized {
            return Err(VoomError::Conflict(format!(
                "artifact_commit_intents receipt: id={id} is {} not authorized",
                current.state.as_str()
            )));
        }
        match (&receipt, &current.receipt) {
            // `applying` opens the journal; every later receipt follows an
            // `applying` journal entry.
            (CommitReceipt::Applying(_), None)
            | (
                CommitReceipt::Applied(_)
                | CommitReceipt::Mismatched(_)
                | CommitReceipt::OutcomeUnknown(_),
                Some(CommitReceipt::Applying(_)),
            ) => {}
            _ => {
                return Err(VoomError::Conflict(format!(
                    "artifact_commit_intents receipt: id={id} cannot record {} after {:?}",
                    receipt.kind_str(),
                    current.receipt.as_ref().map(CommitReceipt::kind_str),
                )));
            }
        }
        let encoded = encode_receipt(&receipt)?;
        let res = sqlx::query(
            "UPDATE artifact_commit_intents \
             SET receipt = ?, intent_epoch = intent_epoch + 1 \
             WHERE id = ? AND state = 'authorized' AND intent_epoch = ?",
        )
        .bind(encoded)
        .bind(checked_id(id.0, "id")?)
        .bind(checked_id(current.intent_epoch, "intent_epoch")?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_intents receipt", e))?;
        changed_intent(res.rows_affected(), id, "receipt")?;
        self.require_intent_in_tx(tx, id).await
    }

    /// Record the current root owner's typed re-observation in the
    /// supplemental-receipt slot (CAS on `intent_epoch`); the original
    /// receipt survives alongside it.
    pub async fn append_supplemental_receipt_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitIntentId,
        receipt: CommitReceipt,
    ) -> Result<ArtifactCommitIntent, VoomError> {
        let current = self.require_intent_in_tx(tx, id).await?;
        if current.state != ArtifactCommitIntentState::RecoveryRequired {
            return Err(VoomError::Conflict(format!(
                "artifact_commit_intents supplemental receipt: id={id} is {} not recovery_required",
                current.state.as_str()
            )));
        }
        let encoded = encode_receipt(&receipt)?;
        let res = sqlx::query(
            "UPDATE artifact_commit_intents \
             SET supplemental_receipt = ?, intent_epoch = intent_epoch + 1 \
             WHERE id = ? AND state = 'recovery_required' AND intent_epoch = ?",
        )
        .bind(encoded)
        .bind(checked_id(id.0, "id")?)
        .bind(checked_id(current.intent_epoch, "intent_epoch")?)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            VoomError::database_context("artifact_commit_intents supplemental receipt", e)
        })?;
        changed_intent(res.rows_affected(), id, "supplemental receipt")?;
        self.require_intent_in_tx(tx, id).await
    }

    /// Transition `authorized -> completed` (CAS on `intent_epoch`),
    /// consuming the fence: the terminal row retains no fence material.
    /// Recovery may complete from `recovery_required`.
    pub async fn mark_completed_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitIntentId,
        now: OffsetDateTime,
    ) -> Result<ArtifactCommitIntent, VoomError> {
        let current = self.require_intent_in_tx(tx, id).await?;
        let terminal_at = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE artifact_commit_intents \
             SET state = 'completed', commit_fence = NULL, terminal_at = ?, \
                 intent_epoch = intent_epoch + 1 \
             WHERE id = ? AND state IN ('authorized', 'recovery_required') AND intent_epoch = ?",
        )
        .bind(&terminal_at)
        .bind(checked_id(id.0, "id")?)
        .bind(checked_id(current.intent_epoch, "intent_epoch")?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_intents complete", e))?;
        changed_intent(res.rows_affected(), id, "complete")?;
        self.require_intent_in_tx(tx, id).await
    }

    /// Transition `authorized -> recovery_required` (CAS on `intent_epoch`)
    /// when drift is observed or recovery classification runs against a
    /// receipt-bearing intent.
    pub async fn mark_recovery_required_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitIntentId,
        now: OffsetDateTime,
    ) -> Result<ArtifactCommitIntent, VoomError> {
        let current = self.require_intent_in_tx(tx, id).await?;
        let terminal_at = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE artifact_commit_intents \
             SET state = 'recovery_required', terminal_at = ?, intent_epoch = intent_epoch + 1 \
             WHERE id = ? AND state = 'authorized' AND intent_epoch = ?",
        )
        .bind(&terminal_at)
        .bind(checked_id(id.0, "id")?)
        .bind(checked_id(current.intent_epoch, "intent_epoch")?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_intents recovery_required", e))?;
        changed_intent(res.rows_affected(), id, "recovery_required")?;
        self.require_intent_in_tx(tx, id).await
    }

    /// Abort a non-terminal intent (CAS on `intent_epoch`). Aborting a
    /// pending intent is always safe (it holds no fence); abort releases
    /// the intent's lease-refusal on its pinned scope and nulls any fence
    /// material so the terminal row retains none.
    pub async fn mark_aborted_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitIntentId,
        now: OffsetDateTime,
    ) -> Result<ArtifactCommitIntent, VoomError> {
        let current = self.require_intent_in_tx(tx, id).await?;
        let terminal_at = iso8601(now)?;
        let res = sqlx::query(
            "UPDATE artifact_commit_intents \
             SET state = 'aborted', commit_fence = NULL, terminal_at = ?, \
                 intent_epoch = intent_epoch + 1 \
             WHERE id = ? AND state IN ('pending', 'authorized', 'recovery_required') \
               AND intent_epoch = ?",
        )
        .bind(&terminal_at)
        .bind(checked_id(id.0, "id")?)
        .bind(checked_id(current.intent_epoch, "intent_epoch")?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_intents abort", e))?;
        changed_intent(res.rows_affected(), id, "abort")?;
        self.require_intent_in_tx(tx, id).await
    }

    /// Non-terminal intents (`pending`/`authorized`/`recovery_required`)
    /// whose pinned target root is currently owned by `node_id` at the
    /// pinned epoch — the node pull listing. A root that has been
    /// reassigned or re-epoched stops advertising its stale intents.
    pub async fn list_open_for_roots_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        node_id: NodeId,
    ) -> Result<Vec<ArtifactCommitIntent>, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_INTENT_COLS.to_owned()
            + " JOIN library_roots r ON r.id = i.target_storage_root_id \
               WHERE i.state IN ('pending', 'authorized', 'recovery_required') \
                 AND r.owner_node_id = ? AND r.root_epoch = i.target_root_epoch \
               ORDER BY i.id ASC";
        let rows = sqlx::query(&sql)
            .bind(checked_id(node_id.0, "node_id")?)
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("artifact_commit_intents open listing", e))?;
        rows.iter().map(row_to_intent).collect()
    }
}

/// Lease-scope consultation for blocking use-lease acquisition: returns a
/// conflict reason when a non-terminal commit intent pins `scope` — its
/// source file version, staging location, or the asset/bundle the pinned
/// artifact handle belongs to. Mirrors `consult_pending_commit_lock_in_tx`
/// scope coverage; the fence stays blocking through recovery, so abort and
/// completion release it.
pub(crate) async fn consult_artifact_intent_lock_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    scope: &LeaseScope,
) -> Result<Option<String>, VoomError> {
    // Column-driven query: each variant filters exactly one column. The
    // asset/bundle granularities resolve through the pinned artifact
    // handle (`artifact_handles.file_asset_id` / `.asset_bundle_id`).
    let (sql, bind_id) = match scope {
        LeaseScope::Version(version_id) => (
            "SELECT i.id, i.state FROM artifact_commit_intents i \
             WHERE i.state IN ('pending', 'authorized', 'recovery_required') \
               AND i.source_file_version_id = ? ORDER BY i.id ASC LIMIT 1",
            version_id.0,
        ),
        LeaseScope::Location(location_id) => (
            "SELECT i.id, i.state FROM artifact_commit_intents i \
             WHERE i.state IN ('pending', 'authorized', 'recovery_required') \
               AND i.staging_location_id = ? ORDER BY i.id ASC LIMIT 1",
            location_id.0,
        ),
        LeaseScope::Asset(asset_id) => (
            "SELECT i.id, i.state FROM artifact_commit_intents i \
             JOIN artifact_handles h ON h.id = i.artifact_handle_id \
             WHERE i.state IN ('pending', 'authorized', 'recovery_required') \
               AND h.file_asset_id = ? ORDER BY i.id ASC LIMIT 1",
            asset_id.0,
        ),
        LeaseScope::Bundle(bundle_id) => (
            "SELECT i.id, i.state FROM artifact_commit_intents i \
             JOIN artifact_handles h ON h.id = i.artifact_handle_id \
             WHERE i.state IN ('pending', 'authorized', 'recovery_required') \
               AND h.asset_bundle_id = ? ORDER BY i.id ASC LIMIT 1",
            bundle_id.0,
        ),
    };
    let row: Option<(i64, String)> = sqlx::query_as(sql)
        .bind(checked_id(bind_id, "scope id")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("consult_artifact_intent_lock", e))?;
    let Some((id, state)) = row else {
        return Ok(None);
    };
    let intent_id = ArtifactCommitIntentId(u64_from_i64(id, "artifact_commit_intents.id")?);
    Ok(Some(format!(
        "artifact_commit_intent {intent_id} ({state}) pins the requested scope"
    )))
}
/// Scan-completion retirement lock for fenced commit intents: returns the
/// first non-terminal intent whose pinned staging location the supplied
/// scan completion would retire (same scope predicate as
/// `SCAN_RECONCILIATION_COMMIT_LOCK_SQL`, joined against
/// `artifact_commit_intents.staging_location_id`). The destructive-commit
/// counterpart protects its scope members; without this mirror a scan
/// completing on a staging root could retire a pinned staging address out
/// from under a live fence.
pub(crate) const SCAN_RECONCILIATION_ARTIFACT_INTENT_LOCK_SQL: &str = "WITH \
     completion_scope(storage_root_id, scan_session_id, high_watermark_id) AS \
     (VALUES (?, ?, ?)) \
     SELECT i.id AS intent_id, i.state AS intent_state, l.id AS location_id \
     FROM artifact_commit_intents AS i \
     JOIN file_locations AS l ON l.id = i.staging_location_id \
     CROSS JOIN completion_scope AS scope \
     WHERE i.state IN ('pending', 'authorized', 'recovery_required') \
       AND l.storage_root_id = scope.storage_root_id \
       AND l.address_state = 'rooted' \
       AND l.retired_at IS NULL \
       AND scope.high_watermark_id IS NOT NULL \
       AND l.id <= scope.high_watermark_id \
       AND NOT EXISTS ( \
           SELECT 1 FROM scan_observations AS observation \
           WHERE observation.scan_session_id = scope.scan_session_id \
             AND observation.provider_relative_locator = l.provider_relative_locator \
       ) \
     ORDER BY i.id ASC, l.id ASC LIMIT 1";

/// Consult [`SCAN_RECONCILIATION_ARTIFACT_INTENT_LOCK_SQL`] on the caller's
/// transaction.
pub(crate) async fn consult_scan_reconciliation_artifact_intent_lock_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    storage_root_id: StorageRootId,
    scan_session_id: voom_core::ids::ScanSessionId,
    location_high_watermark_id: Option<voom_core::FileLocationId>,
) -> Result<Option<(ArtifactCommitIntentId, String, u64)>, VoomError> {
    let row = sqlx::query_as::<_, (i64, String, i64)>(SCAN_RECONCILIATION_ARTIFACT_INTENT_LOCK_SQL)
        .bind(i64_from_u64(
            storage_root_id.0,
            "scan reconciliation storage root ID",
        )?)
        .bind(i64_from_u64(
            scan_session_id.0,
            "scan reconciliation session ID",
        )?)
        .bind(
            location_high_watermark_id
                .map(|id| i64_from_u64(id.0, "scan reconciliation high-water location ID"))
                .transpose()?,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| {
            VoomError::database_context("consult scan reconciliation artifact intent lock", e)
        })?;
    let Some((intent_raw, state, location_raw)) = row else {
        return Ok(None);
    };
    Ok(Some((
        ArtifactCommitIntentId(u64_from_i64(intent_raw, "artifact_commit_intents.id")?),
        state,
        u64_from_i64(location_raw, "file_locations.id")?,
    )))
}

#[cfg(test)]
#[path = "artifact_commit_intents_test.rs"]
mod tests;
