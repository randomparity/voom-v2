use std::collections::HashMap;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqliteConnection, SqlitePool};
use time::OffsetDateTime;
use voom_core::{
    BundleId, EvidenceId, FileAssetId, FileLocationId, FileVersionId, IssueId, IssuePriority,
    IssueSeverity, MediaSnapshotId, MediaVariantId, MediaWorkId, PolicyInputSetId,
    PolicySyntheticTargetId, VoomError,
};
use voom_policy::{
    BundleTargetState, IssueInputState, PolicyInputSetDraft, PolicyInputSourceKind, TargetKind,
    TargetRef,
};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyInputSet {
    pub id: PolicyInputSetId,
    pub slug: String,
    pub display_name: String,
    pub schema_version: u32,
    pub source_kind: PolicyInputSourceKind,
    pub created_at: OffsetDateTime,
    pub description: Option<String>,
    pub epoch: u64,
    pub fixture_labels: Vec<String>,
    pub synthetic_targets: Vec<PolicySyntheticTarget>,
    pub media_snapshots: Vec<PolicyMediaSnapshotInput>,
    pub identity_evidence: Vec<PolicyIdentityEvidenceInput>,
    pub bundle_targets: Vec<PolicyBundleTargetInput>,
    pub quality_profiles: Vec<PolicyQualityProfileSelection>,
    pub issues: Vec<PolicyIssueInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyInputSetSummary {
    pub id: PolicyInputSetId,
    pub slug: String,
    pub display_name: String,
    pub schema_version: u32,
    pub source_kind: PolicyInputSourceKind,
    pub created_at: OffsetDateTime,
    pub description: Option<String>,
    pub epoch: u64,
    pub fixture_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySyntheticTarget {
    pub id: PolicySyntheticTargetId,
    pub synthetic_key: String,
    pub target_kind: TargetKind,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PolicyInputTargetRef {
    MediaWork {
        id: MediaWorkId,
    },
    MediaVariant {
        id: MediaVariantId,
    },
    AssetBundle {
        id: BundleId,
    },
    FileAsset {
        id: FileAssetId,
    },
    FileVersion {
        id: FileVersionId,
    },
    FileLocation {
        id: FileLocationId,
    },
    Synthetic {
        id: PolicySyntheticTargetId,
        key: String,
        kind: TargetKind,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyMediaSnapshotInput {
    pub ordinal: u32,
    pub target: PolicyInputTargetRef,
    pub container: Option<String>,
    pub stream_summary: JsonValue,
    pub video_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub hdr: Option<String>,
    pub bitrate: Option<u64>,
    pub duration_millis: Option<u64>,
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub health_flags: Vec<String>,
    pub existing_media_snapshot_id: Option<MediaSnapshotId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyIdentityEvidenceInput {
    pub ordinal: u32,
    pub target: PolicyInputTargetRef,
    pub assertion_type: String,
    pub provider: String,
    pub provider_version: String,
    pub confidence: f64,
    pub provenance: JsonValue,
    pub observed_at: OffsetDateTime,
    pub existing_evidence_id: Option<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyBundleTargetInput {
    pub ordinal: u32,
    pub target: PolicyInputTargetRef,
    pub role: String,
    pub desired_state: BundleTargetState,
    pub language: Option<String>,
    pub label: Option<String>,
    pub disposition: Option<String>,
    pub artifact_expectation: JsonValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyQualityProfileSelection {
    pub ordinal: u32,
    pub target: PolicyInputTargetRef,
    pub profile_name: String,
    pub profile_version: String,
    pub dimension_weights: JsonValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyIssueInput {
    pub ordinal: u32,
    pub target: PolicyInputTargetRef,
    pub kind: String,
    pub severity: IssueSeverity,
    pub priority: IssuePriority,
    pub state: IssueInputState,
    pub reason: String,
    pub provenance: JsonValue,
    pub existing_issue_id: Option<IssueId>,
}

#[async_trait]
pub trait PolicyInputRepo: Repository {
    async fn create_input_set(
        &self,
        input: PolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError>;

    async fn create_input_set_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: PolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError>;

    async fn get_input_set(
        &self,
        id: PolicyInputSetId,
    ) -> Result<Option<PolicyInputSet>, VoomError>;
    async fn get_input_set_by_slug(&self, slug: &str) -> Result<Option<PolicyInputSet>, VoomError>;
    async fn list_input_sets(&self) -> Result<Vec<PolicyInputSetSummary>, VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqlitePolicyInputRepo {
    pool: SqlitePool,
}

impl SqlitePolicyInputRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqlitePolicyInputRepo {}

#[async_trait]
#[expect(
    clippy::too_many_lines,
    reason = "transactional create mirrors the policy input schema tables in one atomic write"
)]
impl PolicyInputRepo for SqlitePolicyInputRepo {
    async fn create_input_set(
        &self,
        input: PolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.create_input_set_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn create_input_set_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: PolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError> {
        voom_policy::validate_input_set(&input)
            .map_err(|e| VoomError::PolicyValidationError(format!("{e:?}")))?;

        let created_at = iso8601(input.created_at)?;
        let res = sqlx::query(
            "INSERT INTO policy_input_sets \
             (slug, display_name, schema_version, source_kind, created_at, description) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&input.slug)
        .bind(&input.display_name)
        .bind(i64::from(input.schema_version))
        .bind(source_kind_as_str(input.source_kind))
        .bind(&created_at)
        .bind(&input.description)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("policy_input_sets insert: {e}")))?;
        let set_id = PolicyInputSetId(u64_from_i64(res.last_insert_rowid()));

        for label in &input.fixture_labels {
            sqlx::query(
                "INSERT INTO policy_input_set_fixture_labels (policy_input_set_id, label) \
                 VALUES (?, ?)",
            )
            .bind(i64_from_u64(set_id.0))
            .bind(label)
            .execute(&mut **tx)
            .await
            .map_err(|e| {
                VoomError::Database(format!("policy_input_set_fixture_labels insert: {e}"))
            })?;
        }

        let mut synthetic_target_ids = HashMap::new();
        for target in &input.synthetic_targets {
            let res = sqlx::query(
                "INSERT INTO policy_input_synthetic_targets \
                 (policy_input_set_id, synthetic_key, target_kind, display_name) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(i64_from_u64(set_id.0))
            .bind(&target.synthetic_key)
            .bind(target_kind_as_str(target.target_kind))
            .bind(&target.display_name)
            .execute(&mut **tx)
            .await
            .map_err(|e| {
                VoomError::Database(format!("policy_input_synthetic_targets insert: {e}"))
            })?;
            synthetic_target_ids.insert(
                (target.synthetic_key.clone(), target.target_kind),
                PolicySyntheticTargetId(u64_from_i64(res.last_insert_rowid())),
            );
        }

        for snapshot in &input.media_snapshots {
            let ids = PersistedTargetIds::from_ref(&snapshot.target, &synthetic_target_ids)?;
            sqlx::query(
                "INSERT INTO policy_media_snapshot_inputs \
                 (policy_input_set_id, ordinal, media_work_id, media_variant_id, asset_bundle_id, \
                  file_asset_id, file_version_id, file_location_id, synthetic_target_id, container, \
                  stream_summary, video_codec, width, height, hdr, bitrate, duration_millis, \
                  audio_languages, subtitle_languages, health_flags, existing_media_snapshot_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(i64_from_u64(set_id.0))
            .bind(i64::from(snapshot.ordinal))
            .bind(ids.media_work_id)
            .bind(ids.media_variant_id)
            .bind(ids.asset_bundle_id)
            .bind(ids.file_asset_id)
            .bind(ids.file_version_id)
            .bind(ids.file_location_id)
            .bind(ids.synthetic_target_id)
            .bind(&snapshot.container)
            .bind(serialize_json(&snapshot.stream_summary, "stream_summary")?)
            .bind(&snapshot.video_codec)
            .bind(snapshot.width.map(i64::from))
            .bind(snapshot.height.map(i64::from))
            .bind(&snapshot.hdr)
            .bind(snapshot.bitrate.map(i64_from_u64))
            .bind(snapshot.duration_millis.map(i64_from_u64))
            .bind(json_string(&snapshot.audio_languages, "audio_languages")?)
            .bind(json_string(&snapshot.subtitle_languages, "subtitle_languages")?)
            .bind(json_string(&snapshot.health_flags, "health_flags")?)
            .bind(snapshot.existing_media_snapshot_id.map(|id| i64_from_u64(id.0)))
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("policy_media_snapshot_inputs insert: {e}")))?;
        }

        for evidence in &input.identity_evidence {
            let ids = PersistedTargetIds::from_ref(&evidence.target, &synthetic_target_ids)?;
            sqlx::query(
                "INSERT INTO policy_identity_evidence_inputs \
                 (policy_input_set_id, ordinal, media_work_id, media_variant_id, asset_bundle_id, \
                  file_asset_id, file_version_id, file_location_id, synthetic_target_id, assertion_type, \
                  provider, provider_version, confidence, provenance, observed_at, existing_evidence_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(i64_from_u64(set_id.0))
            .bind(i64::from(evidence.ordinal))
            .bind(ids.media_work_id)
            .bind(ids.media_variant_id)
            .bind(ids.asset_bundle_id)
            .bind(ids.file_asset_id)
            .bind(ids.file_version_id)
            .bind(ids.file_location_id)
            .bind(ids.synthetic_target_id)
            .bind(&evidence.assertion_type)
            .bind(&evidence.provider)
            .bind(&evidence.provider_version)
            .bind(evidence.confidence)
            .bind(serialize_json(&evidence.provenance, "provenance")?)
            .bind(iso8601(evidence.observed_at)?)
            .bind(evidence.existing_evidence_id.map(|id| i64_from_u64(id.0)))
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("policy_identity_evidence_inputs insert: {e}")))?;
        }

        for bundle in &input.bundle_targets {
            let ids = PersistedTargetIds::from_ref(&bundle.target, &synthetic_target_ids)?;
            sqlx::query(
                "INSERT INTO policy_bundle_target_inputs \
                 (policy_input_set_id, ordinal, media_work_id, media_variant_id, asset_bundle_id, \
                  file_asset_id, file_version_id, file_location_id, synthetic_target_id, role, \
                  desired_state, language, label, disposition, artifact_expectation) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(i64_from_u64(set_id.0))
            .bind(i64::from(bundle.ordinal))
            .bind(ids.media_work_id)
            .bind(ids.media_variant_id)
            .bind(ids.asset_bundle_id)
            .bind(ids.file_asset_id)
            .bind(ids.file_version_id)
            .bind(ids.file_location_id)
            .bind(ids.synthetic_target_id)
            .bind(&bundle.role)
            .bind(bundle_target_state_as_str(bundle.desired_state))
            .bind(&bundle.language)
            .bind(&bundle.label)
            .bind(&bundle.disposition)
            .bind(serialize_json(
                &bundle.artifact_expectation,
                "artifact_expectation",
            )?)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("policy_bundle_target_inputs insert: {e}")))?;
        }

        for profile in &input.quality_profiles {
            let ids = PersistedTargetIds::from_ref(&profile.target, &synthetic_target_ids)?;
            sqlx::query(
                "INSERT INTO policy_quality_profile_selections \
                 (policy_input_set_id, ordinal, media_work_id, media_variant_id, asset_bundle_id, \
                  file_asset_id, file_version_id, file_location_id, synthetic_target_id, \
                  profile_name, profile_version, dimension_weights) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(i64_from_u64(set_id.0))
            .bind(i64::from(profile.ordinal))
            .bind(ids.media_work_id)
            .bind(ids.media_variant_id)
            .bind(ids.asset_bundle_id)
            .bind(ids.file_asset_id)
            .bind(ids.file_version_id)
            .bind(ids.file_location_id)
            .bind(ids.synthetic_target_id)
            .bind(&profile.profile_name)
            .bind(&profile.profile_version)
            .bind(serialize_json(
                &profile.dimension_weights,
                "dimension_weights",
            )?)
            .execute(&mut **tx)
            .await
            .map_err(|e| {
                VoomError::Database(format!("policy_quality_profile_selections insert: {e}"))
            })?;
        }

        for issue in &input.issues {
            let ids = PersistedTargetIds::from_ref(&issue.target, &synthetic_target_ids)?;
            sqlx::query(
                "INSERT INTO policy_issue_inputs \
                 (policy_input_set_id, ordinal, media_work_id, media_variant_id, asset_bundle_id, \
                  file_asset_id, file_version_id, file_location_id, synthetic_target_id, kind, \
                  severity, priority, state, reason, provenance, existing_issue_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(i64_from_u64(set_id.0))
            .bind(i64::from(issue.ordinal))
            .bind(ids.media_work_id)
            .bind(ids.media_variant_id)
            .bind(ids.asset_bundle_id)
            .bind(ids.file_asset_id)
            .bind(ids.file_version_id)
            .bind(ids.file_location_id)
            .bind(ids.synthetic_target_id)
            .bind(&issue.kind)
            .bind(issue.severity.as_str())
            .bind(issue.priority.as_str())
            .bind(issue_input_state_as_str(issue.state))
            .bind(&issue.reason)
            .bind(serialize_json(&issue.provenance, "provenance")?)
            .bind(issue.existing_issue_id.map(|id| i64_from_u64(id.0)))
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("policy_issue_inputs insert: {e}")))?;
        }

        get_input_set_in_tx(tx, set_id).await?.ok_or_else(|| {
            VoomError::Internal(format!("policy_input_sets post-insert get: {set_id}"))
        })
    }

    async fn get_input_set(
        &self,
        id: PolicyInputSetId,
    ) -> Result<Option<PolicyInputSet>, VoomError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| VoomError::Database(format!("acquire: {e}")))?;
        get_input_set_by_id_conn(&mut conn, id).await
    }

    async fn get_input_set_by_slug(&self, slug: &str) -> Result<Option<PolicyInputSet>, VoomError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| VoomError::Database(format!("acquire: {e}")))?;
        let row = sqlx::query(ROOT_SELECT_SLUG)
            .bind(slug)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| VoomError::Database(format!("policy_input_sets get by slug: {e}")))?;
        match row.as_ref().map(row_to_root).transpose()? {
            Some(root) => hydrate_input_set(&mut conn, root).await.map(Some),
            None => Ok(None),
        }
    }

    async fn list_input_sets(&self) -> Result<Vec<PolicyInputSetSummary>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, slug, display_name, schema_version, source_kind, created_at, description, epoch \
             FROM policy_input_sets ORDER BY slug ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("policy_input_sets list: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let root = row_to_root(row)?;
            let mut conn = self
                .pool
                .acquire()
                .await
                .map_err(|e| VoomError::Database(format!("acquire: {e}")))?;
            let fixture_labels = load_fixture_labels(&mut conn, root.id).await?;
            out.push(PolicyInputSetSummary {
                id: root.id,
                slug: root.slug,
                display_name: root.display_name,
                schema_version: root.schema_version,
                source_kind: root.source_kind,
                created_at: root.created_at,
                description: root.description,
                epoch: root.epoch,
                fixture_labels,
            });
        }
        Ok(out)
    }
}

const ROOT_SELECT: &str = "SELECT id, slug, display_name, schema_version, source_kind, created_at, description, epoch \
    FROM policy_input_sets WHERE id = ?";
const ROOT_SELECT_SLUG: &str = "SELECT id, slug, display_name, schema_version, source_kind, created_at, description, epoch \
    FROM policy_input_sets WHERE slug = ?";

async fn get_input_set_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: PolicyInputSetId,
) -> Result<Option<PolicyInputSet>, VoomError> {
    let row = sqlx::query(ROOT_SELECT)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("policy_input_sets get: {e}")))?;
    match row.as_ref().map(row_to_root).transpose()? {
        Some(root) => hydrate_input_set(tx, root).await.map(Some),
        None => Ok(None),
    }
}

async fn get_input_set_by_id_conn(
    conn: &mut SqliteConnection,
    id: PolicyInputSetId,
) -> Result<Option<PolicyInputSet>, VoomError> {
    let row = sqlx::query(ROOT_SELECT)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| VoomError::Database(format!("policy_input_sets get: {e}")))?;
    match row.as_ref().map(row_to_root).transpose()? {
        Some(root) => hydrate_input_set(conn, root).await.map(Some),
        None => Ok(None),
    }
}

#[derive(Debug)]
struct RootRow {
    id: PolicyInputSetId,
    slug: String,
    display_name: String,
    schema_version: u32,
    source_kind: PolicyInputSourceKind,
    created_at: OffsetDateTime,
    description: Option<String>,
    epoch: u64,
}

async fn hydrate_input_set(
    conn: &mut SqliteConnection,
    root: RootRow,
) -> Result<PolicyInputSet, VoomError> {
    let synthetic_targets = load_synthetic_targets(conn, root.id).await?;
    Ok(PolicyInputSet {
        id: root.id,
        slug: root.slug,
        display_name: root.display_name,
        schema_version: root.schema_version,
        source_kind: root.source_kind,
        created_at: root.created_at,
        description: root.description,
        epoch: root.epoch,
        fixture_labels: load_fixture_labels(conn, root.id).await?,
        media_snapshots: load_media_snapshots(conn, root.id).await?,
        identity_evidence: load_identity_evidence(conn, root.id).await?,
        bundle_targets: load_bundle_targets(conn, root.id).await?,
        quality_profiles: load_quality_profiles(conn, root.id).await?,
        issues: load_issues(conn, root.id).await?,
        synthetic_targets,
    })
}

async fn load_fixture_labels(
    conn: &mut SqliteConnection,
    set_id: PolicyInputSetId,
) -> Result<Vec<String>, VoomError> {
    let rows = sqlx::query(
        "SELECT label FROM policy_input_set_fixture_labels \
         WHERE policy_input_set_id = ? ORDER BY label ASC",
    )
    .bind(i64_from_u64(set_id.0))
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| VoomError::Database(format!("policy_input_set_fixture_labels list: {e}")))?;
    rows.iter()
        .map(|row| {
            row.try_get("label")
                .map_err(|e| map_row_err("policy_input_set_fixture_labels", &e))
        })
        .collect()
}

async fn load_synthetic_targets(
    conn: &mut SqliteConnection,
    set_id: PolicyInputSetId,
) -> Result<Vec<PolicySyntheticTarget>, VoomError> {
    let rows = sqlx::query(
        "SELECT id, synthetic_key, target_kind, display_name \
         FROM policy_input_synthetic_targets \
         WHERE policy_input_set_id = ? ORDER BY synthetic_key ASC, id ASC",
    )
    .bind(i64_from_u64(set_id.0))
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| VoomError::Database(format!("policy_input_synthetic_targets list: {e}")))?;
    rows.iter().map(row_to_synthetic_target).collect()
}

async fn load_media_snapshots(
    conn: &mut SqliteConnection,
    set_id: PolicyInputSetId,
) -> Result<Vec<PolicyMediaSnapshotInput>, VoomError> {
    let rows = sqlx::query(
        "SELECT c.ordinal, c.media_work_id, c.media_variant_id, c.asset_bundle_id, c.file_asset_id, \
                c.file_version_id, c.file_location_id, c.synthetic_target_id, st.synthetic_key, st.target_kind, \
                c.container, c.stream_summary, c.video_codec, c.width, c.height, c.hdr, c.bitrate, \
                c.duration_millis, c.audio_languages, c.subtitle_languages, c.health_flags, \
                c.existing_media_snapshot_id \
         FROM policy_media_snapshot_inputs c \
         LEFT JOIN policy_input_synthetic_targets st ON st.id = c.synthetic_target_id \
         WHERE c.policy_input_set_id = ? ORDER BY c.ordinal ASC",
    )
    .bind(i64_from_u64(set_id.0))
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| VoomError::Database(format!("policy_media_snapshot_inputs list: {e}")))?;
    rows.iter().map(row_to_media_snapshot).collect()
}

async fn load_identity_evidence(
    conn: &mut SqliteConnection,
    set_id: PolicyInputSetId,
) -> Result<Vec<PolicyIdentityEvidenceInput>, VoomError> {
    let rows = sqlx::query(
        "SELECT c.ordinal, c.media_work_id, c.media_variant_id, c.asset_bundle_id, c.file_asset_id, \
                c.file_version_id, c.file_location_id, c.synthetic_target_id, st.synthetic_key, st.target_kind, \
                c.assertion_type, c.provider, c.provider_version, c.confidence, c.provenance, \
                c.observed_at, c.existing_evidence_id \
         FROM policy_identity_evidence_inputs c \
         LEFT JOIN policy_input_synthetic_targets st ON st.id = c.synthetic_target_id \
         WHERE c.policy_input_set_id = ? ORDER BY c.ordinal ASC",
    )
    .bind(i64_from_u64(set_id.0))
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| VoomError::Database(format!("policy_identity_evidence_inputs list: {e}")))?;
    rows.iter().map(row_to_identity_evidence).collect()
}

async fn load_bundle_targets(
    conn: &mut SqliteConnection,
    set_id: PolicyInputSetId,
) -> Result<Vec<PolicyBundleTargetInput>, VoomError> {
    let rows = sqlx::query(
        "SELECT c.ordinal, c.media_work_id, c.media_variant_id, c.asset_bundle_id, c.file_asset_id, \
                c.file_version_id, c.file_location_id, c.synthetic_target_id, st.synthetic_key, st.target_kind, \
                c.role, c.desired_state, c.language, c.label, c.disposition, c.artifact_expectation \
         FROM policy_bundle_target_inputs c \
         LEFT JOIN policy_input_synthetic_targets st ON st.id = c.synthetic_target_id \
         WHERE c.policy_input_set_id = ? ORDER BY c.ordinal ASC",
    )
    .bind(i64_from_u64(set_id.0))
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| VoomError::Database(format!("policy_bundle_target_inputs list: {e}")))?;
    rows.iter().map(row_to_bundle_target).collect()
}

async fn load_quality_profiles(
    conn: &mut SqliteConnection,
    set_id: PolicyInputSetId,
) -> Result<Vec<PolicyQualityProfileSelection>, VoomError> {
    let rows = sqlx::query(
        "SELECT c.ordinal, c.media_work_id, c.media_variant_id, c.asset_bundle_id, c.file_asset_id, \
                c.file_version_id, c.file_location_id, c.synthetic_target_id, st.synthetic_key, st.target_kind, \
                c.profile_name, c.profile_version, c.dimension_weights \
         FROM policy_quality_profile_selections c \
         LEFT JOIN policy_input_synthetic_targets st ON st.id = c.synthetic_target_id \
         WHERE c.policy_input_set_id = ? ORDER BY c.ordinal ASC",
    )
    .bind(i64_from_u64(set_id.0))
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| VoomError::Database(format!("policy_quality_profile_selections list: {e}")))?;
    rows.iter().map(row_to_quality_profile).collect()
}

async fn load_issues(
    conn: &mut SqliteConnection,
    set_id: PolicyInputSetId,
) -> Result<Vec<PolicyIssueInput>, VoomError> {
    let rows = sqlx::query(
        "SELECT c.ordinal, c.media_work_id, c.media_variant_id, c.asset_bundle_id, c.file_asset_id, \
                c.file_version_id, c.file_location_id, c.synthetic_target_id, st.synthetic_key, st.target_kind, \
                c.kind, c.severity, c.priority, c.state, c.reason, c.provenance, c.existing_issue_id \
         FROM policy_issue_inputs c \
         LEFT JOIN policy_input_synthetic_targets st ON st.id = c.synthetic_target_id \
         WHERE c.policy_input_set_id = ? ORDER BY c.ordinal ASC",
    )
    .bind(i64_from_u64(set_id.0))
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| VoomError::Database(format!("policy_issue_inputs list: {e}")))?;
    rows.iter().map(row_to_issue).collect()
}

fn row_to_root(row: &sqlx::sqlite::SqliteRow) -> Result<RootRow, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("policy_input_sets", &e))?;
    let schema_version: i64 = row
        .try_get("schema_version")
        .map_err(|e| map_row_err("policy_input_sets", &e))?;
    let source_kind: String = row
        .try_get("source_kind")
        .map_err(|e| map_row_err("policy_input_sets", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("policy_input_sets", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("policy_input_sets", &e))?;
    Ok(RootRow {
        id: PolicyInputSetId(u64_from_i64(id)),
        slug: row
            .try_get("slug")
            .map_err(|e| map_row_err("policy_input_sets", &e))?,
        display_name: row
            .try_get("display_name")
            .map_err(|e| map_row_err("policy_input_sets", &e))?,
        schema_version: u32_from_i64(schema_version)?,
        source_kind: parse_wire(&source_kind, "policy_input_sets.source_kind")?,
        created_at: parse_iso8601(&created_at)?,
        description: row
            .try_get("description")
            .map_err(|e| map_row_err("policy_input_sets", &e))?,
        epoch: u64_from_i64(epoch),
    })
}

fn row_to_synthetic_target(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PolicySyntheticTarget, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("policy_input_synthetic_targets", &e))?;
    let target_kind: String = row
        .try_get("target_kind")
        .map_err(|e| map_row_err("policy_input_synthetic_targets", &e))?;
    Ok(PolicySyntheticTarget {
        id: PolicySyntheticTargetId(u64_from_i64(id)),
        synthetic_key: row
            .try_get("synthetic_key")
            .map_err(|e| map_row_err("policy_input_synthetic_targets", &e))?,
        target_kind: parse_wire(&target_kind, "policy_input_synthetic_targets.target_kind")?,
        display_name: row
            .try_get("display_name")
            .map_err(|e| map_row_err("policy_input_synthetic_targets", &e))?,
    })
}

fn row_to_media_snapshot(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PolicyMediaSnapshotInput, VoomError> {
    Ok(PolicyMediaSnapshotInput {
        ordinal: ordinal(row, "policy_media_snapshot_inputs")?,
        target: target_ref_from_row(row, "policy_media_snapshot_inputs")?,
        container: row
            .try_get("container")
            .map_err(|e| map_row_err("policy_media_snapshot_inputs", &e))?,
        stream_summary: json_value(row, "stream_summary", "policy_media_snapshot_inputs")?,
        video_codec: row
            .try_get("video_codec")
            .map_err(|e| map_row_err("policy_media_snapshot_inputs", &e))?,
        width: optional_u32(row, "width", "policy_media_snapshot_inputs")?,
        height: optional_u32(row, "height", "policy_media_snapshot_inputs")?,
        hdr: row
            .try_get("hdr")
            .map_err(|e| map_row_err("policy_media_snapshot_inputs", &e))?,
        bitrate: optional_id(row, "bitrate", "policy_media_snapshot_inputs")?,
        duration_millis: optional_id(row, "duration_millis", "policy_media_snapshot_inputs")?,
        audio_languages: json_value(row, "audio_languages", "policy_media_snapshot_inputs")?,
        subtitle_languages: json_value(row, "subtitle_languages", "policy_media_snapshot_inputs")?,
        health_flags: json_value(row, "health_flags", "policy_media_snapshot_inputs")?,
        existing_media_snapshot_id: optional_id(
            row,
            "existing_media_snapshot_id",
            "policy_media_snapshot_inputs",
        )?
        .map(MediaSnapshotId),
    })
}

fn row_to_identity_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PolicyIdentityEvidenceInput, VoomError> {
    let observed_at: String = row
        .try_get("observed_at")
        .map_err(|e| map_row_err("policy_identity_evidence_inputs", &e))?;
    Ok(PolicyIdentityEvidenceInput {
        ordinal: ordinal(row, "policy_identity_evidence_inputs")?,
        target: target_ref_from_row(row, "policy_identity_evidence_inputs")?,
        assertion_type: row
            .try_get("assertion_type")
            .map_err(|e| map_row_err("policy_identity_evidence_inputs", &e))?,
        provider: row
            .try_get("provider")
            .map_err(|e| map_row_err("policy_identity_evidence_inputs", &e))?,
        provider_version: row
            .try_get("provider_version")
            .map_err(|e| map_row_err("policy_identity_evidence_inputs", &e))?,
        confidence: row
            .try_get("confidence")
            .map_err(|e| map_row_err("policy_identity_evidence_inputs", &e))?,
        provenance: json_value(row, "provenance", "policy_identity_evidence_inputs")?,
        observed_at: parse_iso8601(&observed_at)?,
        existing_evidence_id: optional_id(
            row,
            "existing_evidence_id",
            "policy_identity_evidence_inputs",
        )?
        .map(EvidenceId),
    })
}

fn row_to_bundle_target(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PolicyBundleTargetInput, VoomError> {
    let desired_state: String = row
        .try_get("desired_state")
        .map_err(|e| map_row_err("policy_bundle_target_inputs", &e))?;
    Ok(PolicyBundleTargetInput {
        ordinal: ordinal(row, "policy_bundle_target_inputs")?,
        target: target_ref_from_row(row, "policy_bundle_target_inputs")?,
        role: row
            .try_get("role")
            .map_err(|e| map_row_err("policy_bundle_target_inputs", &e))?,
        desired_state: parse_wire(&desired_state, "policy_bundle_target_inputs.desired_state")?,
        language: row
            .try_get("language")
            .map_err(|e| map_row_err("policy_bundle_target_inputs", &e))?,
        label: row
            .try_get("label")
            .map_err(|e| map_row_err("policy_bundle_target_inputs", &e))?,
        disposition: row
            .try_get("disposition")
            .map_err(|e| map_row_err("policy_bundle_target_inputs", &e))?,
        artifact_expectation: json_value(
            row,
            "artifact_expectation",
            "policy_bundle_target_inputs",
        )?,
    })
}

fn row_to_quality_profile(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PolicyQualityProfileSelection, VoomError> {
    Ok(PolicyQualityProfileSelection {
        ordinal: ordinal(row, "policy_quality_profile_selections")?,
        target: target_ref_from_row(row, "policy_quality_profile_selections")?,
        profile_name: row
            .try_get("profile_name")
            .map_err(|e| map_row_err("policy_quality_profile_selections", &e))?,
        profile_version: row
            .try_get("profile_version")
            .map_err(|e| map_row_err("policy_quality_profile_selections", &e))?,
        dimension_weights: json_value(
            row,
            "dimension_weights",
            "policy_quality_profile_selections",
        )?,
    })
}

fn row_to_issue(row: &sqlx::sqlite::SqliteRow) -> Result<PolicyIssueInput, VoomError> {
    let severity: String = row
        .try_get("severity")
        .map_err(|e| map_row_err("policy_issue_inputs", &e))?;
    let priority: String = row
        .try_get("priority")
        .map_err(|e| map_row_err("policy_issue_inputs", &e))?;
    let state: String = row
        .try_get("state")
        .map_err(|e| map_row_err("policy_issue_inputs", &e))?;
    Ok(PolicyIssueInput {
        ordinal: ordinal(row, "policy_issue_inputs")?,
        target: target_ref_from_row(row, "policy_issue_inputs")?,
        kind: row
            .try_get("kind")
            .map_err(|e| map_row_err("policy_issue_inputs", &e))?,
        severity: IssueSeverity::parse(&severity)?,
        priority: IssuePriority::parse(&priority)?,
        state: parse_wire(&state, "policy_issue_inputs.state")?,
        reason: row
            .try_get("reason")
            .map_err(|e| map_row_err("policy_issue_inputs", &e))?,
        provenance: json_value(row, "provenance", "policy_issue_inputs")?,
        existing_issue_id: optional_id(row, "existing_issue_id", "policy_issue_inputs")?
            .map(IssueId),
    })
}

#[derive(Debug)]
#[expect(
    clippy::struct_field_names,
    reason = "field names mirror the target id columns inserted into each child table"
)]
struct PersistedTargetIds {
    media_work_id: Option<i64>,
    media_variant_id: Option<i64>,
    asset_bundle_id: Option<i64>,
    file_asset_id: Option<i64>,
    file_version_id: Option<i64>,
    file_location_id: Option<i64>,
    synthetic_target_id: Option<i64>,
}

impl PersistedTargetIds {
    fn from_ref(
        target: &TargetRef,
        synthetic_target_ids: &HashMap<(String, TargetKind), PolicySyntheticTargetId>,
    ) -> Result<Self, VoomError> {
        let empty = Self {
            media_work_id: None,
            media_variant_id: None,
            asset_bundle_id: None,
            file_asset_id: None,
            file_version_id: None,
            file_location_id: None,
            synthetic_target_id: None,
        };
        Ok(match target {
            TargetRef::MediaWork { id } => Self {
                media_work_id: Some(i64_from_u64(id.0)),
                ..empty
            },
            TargetRef::MediaVariant { id } => Self {
                media_variant_id: Some(i64_from_u64(id.0)),
                ..empty
            },
            TargetRef::AssetBundle { id } => Self {
                asset_bundle_id: Some(i64_from_u64(id.0)),
                ..empty
            },
            TargetRef::FileAsset { id } => Self {
                file_asset_id: Some(i64_from_u64(id.0)),
                ..empty
            },
            TargetRef::FileVersion { id } => Self {
                file_version_id: Some(i64_from_u64(id.0)),
                ..empty
            },
            TargetRef::FileLocation { id } => Self {
                file_location_id: Some(i64_from_u64(id.0)),
                ..empty
            },
            TargetRef::Synthetic { key, kind } => {
                let id = synthetic_target_ids
                    .get(&(key.clone(), *kind))
                    .ok_or_else(|| {
                        VoomError::Internal(format!(
                            "synthetic target id missing for {key:?}/{kind:?}"
                        ))
                    })?;
                Self {
                    synthetic_target_id: Some(i64_from_u64(id.0)),
                    ..empty
                }
            }
        })
    }
}

fn target_ref_from_row(
    row: &sqlx::sqlite::SqliteRow,
    table: &'static str,
) -> Result<PolicyInputTargetRef, VoomError> {
    if let Some(id) = optional_id(row, "media_work_id", table)? {
        return Ok(PolicyInputTargetRef::MediaWork {
            id: MediaWorkId(id),
        });
    }
    if let Some(id) = optional_id(row, "media_variant_id", table)? {
        return Ok(PolicyInputTargetRef::MediaVariant {
            id: MediaVariantId(id),
        });
    }
    if let Some(id) = optional_id(row, "asset_bundle_id", table)? {
        return Ok(PolicyInputTargetRef::AssetBundle { id: BundleId(id) });
    }
    if let Some(id) = optional_id(row, "file_asset_id", table)? {
        return Ok(PolicyInputTargetRef::FileAsset {
            id: FileAssetId(id),
        });
    }
    if let Some(id) = optional_id(row, "file_version_id", table)? {
        return Ok(PolicyInputTargetRef::FileVersion {
            id: FileVersionId(id),
        });
    }
    if let Some(id) = optional_id(row, "file_location_id", table)? {
        return Ok(PolicyInputTargetRef::FileLocation {
            id: FileLocationId(id),
        });
    }
    let id = optional_id(row, "synthetic_target_id", table)?
        .ok_or_else(|| VoomError::Database(format!("{table} target shape missing")))?;
    let key: String = row
        .try_get("synthetic_key")
        .map_err(|e| map_row_err(table, &e))?;
    let kind: String = row
        .try_get("target_kind")
        .map_err(|e| map_row_err(table, &e))?;
    Ok(PolicyInputTargetRef::Synthetic {
        id: PolicySyntheticTargetId(id),
        key,
        kind: parse_wire(&kind, "policy_input_synthetic_targets.target_kind")?,
    })
}

fn ordinal(row: &sqlx::sqlite::SqliteRow, table: &'static str) -> Result<u32, VoomError> {
    let ordinal: i64 = row.try_get("ordinal").map_err(|e| map_row_err(table, &e))?;
    u32_from_i64(ordinal)
}

fn optional_id(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
    table: &'static str,
) -> Result<Option<u64>, VoomError> {
    let value: Option<i64> = row.try_get(column).map_err(|e| map_row_err(table, &e))?;
    Ok(value.map(u64_from_i64))
}

fn optional_u32(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
    table: &'static str,
) -> Result<Option<u32>, VoomError> {
    let value: Option<i64> = row.try_get(column).map_err(|e| map_row_err(table, &e))?;
    value.map(u32_from_i64).transpose()
}

fn json_value<T: DeserializeOwned>(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
    table: &'static str,
) -> Result<T, VoomError> {
    let raw: String = row.try_get(column).map_err(|e| map_row_err(table, &e))?;
    serde_json::from_str(&raw)
        .map_err(|e| VoomError::Database(format!("{table}.{column} JSON: {e}")))
}

fn json_string<T: serde::Serialize>(value: &T, field: &'static str) -> Result<String, VoomError> {
    serde_json::to_string(value).map_err(|e| VoomError::Internal(format!("serialize {field}: {e}")))
}

fn parse_wire<T: DeserializeOwned>(value: &str, field: &'static str) -> Result<T, VoomError> {
    serde_json::from_value(JsonValue::String(value.to_owned()))
        .map_err(|e| VoomError::Database(format!("{field} {value:?} not in vocab: {e}")))
}

fn source_kind_as_str(value: PolicyInputSourceKind) -> &'static str {
    match value {
        PolicyInputSourceKind::Fixture => "fixture",
        PolicyInputSourceKind::Test => "test",
        PolicyInputSourceKind::Imported => "imported",
        PolicyInputSourceKind::Manual => "manual",
    }
}

fn target_kind_as_str(value: TargetKind) -> &'static str {
    match value {
        TargetKind::MediaWork => "media_work",
        TargetKind::MediaVariant => "media_variant",
        TargetKind::AssetBundle => "asset_bundle",
        TargetKind::FileAsset => "file_asset",
        TargetKind::FileVersion => "file_version",
        TargetKind::FileLocation => "file_location",
    }
}

fn bundle_target_state_as_str(value: BundleTargetState) -> &'static str {
    match value {
        BundleTargetState::Required => "required",
        BundleTargetState::Allowed => "allowed",
        BundleTargetState::Forbidden => "forbidden",
        BundleTargetState::Preferred => "preferred",
    }
}

fn issue_input_state_as_str(value: IssueInputState) -> &'static str {
    match value {
        IssueInputState::Open => "open",
        IssueInputState::Accepted => "accepted",
        IssueInputState::Suppressed => "suppressed",
        IssueInputState::Planned => "planned",
    }
}

#[cfg(test)]
#[path = "policy_inputs_test.rs"]
mod tests;
