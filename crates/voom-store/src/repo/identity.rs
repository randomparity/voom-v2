//! `IdentityRepo` — owns `media_works`, `media_variants`, `file_assets`,
//! `file_versions`, `file_locations`, `identity_evidence`, and
//! `media_snapshots`. The ingest entry point is
//! `record_discovered_file_in_tx`; rename reconciliation is
//! `reconcile_rename_in_tx`. Both are documented in spec §8.7.
//!
//! `asset_bundles` / `asset_bundle_members` live in their own repo
//! (`BundleRepo`) because the membership UNIQUE constraint and the
//! variant scoping make the surface noticeably different.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{
    EvidenceId, FileAssetId, FileLocationId, FileVersionId, MediaSnapshotId, MediaVariantId,
    MediaWorkId, VoomError, WorkerId,
};
use voom_events::AssertionKind;

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
};

// ---------- value-type vocabularies ----------------------------------------

/// `media_works.kind` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaWorkKind {
    Movie,
    Episode,
    Personal,
    Unknown,
}

impl MediaWorkKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Episode => "episode",
            Self::Personal => "personal",
            Self::Unknown => "unknown",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "movie" => Ok(Self::Movie),
            "episode" => Ok(Self::Episode),
            "personal" => Ok(Self::Personal),
            "unknown" => Ok(Self::Unknown),
            other => Err(VoomError::Database(format!(
                "media_works.kind {other:?} not in vocab"
            ))),
        }
    }
}

/// `file_locations.kind` vocabulary. Mirrors the SQL CHECK exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileLocationKind {
    LocalPath,
    SharedMount,
    ObjectStoreKey,
    BackupPath,
    Historical,
}

impl FileLocationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalPath => "local_path",
            Self::SharedMount => "shared_mount",
            Self::ObjectStoreKey => "object_store_key",
            Self::BackupPath => "backup_path",
            Self::Historical => "historical",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "local_path" => Ok(Self::LocalPath),
            "shared_mount" => Ok(Self::SharedMount),
            "object_store_key" => Ok(Self::ObjectStoreKey),
            "backup_path" => Ok(Self::BackupPath),
            "historical" => Ok(Self::Historical),
            other => Err(VoomError::Database(format!(
                "file_locations.kind {other:?} not in vocab"
            ))),
        }
    }
}

/// `file_versions.produced_by` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProducedBy {
    Ingest,
    Transcode,
    Remux,
    Restore,
    ExternalObserved,
}

impl ProducedBy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Transcode => "transcode",
            Self::Remux => "remux",
            Self::Restore => "restore",
            Self::ExternalObserved => "external_observed",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "ingest" => Ok(Self::Ingest),
            "transcode" => Ok(Self::Transcode),
            "remux" => Ok(Self::Remux),
            "restore" => Ok(Self::Restore),
            "external_observed" => Ok(Self::ExternalObserved),
            other => Err(VoomError::Database(format!(
                "file_versions.produced_by {other:?} not in vocab"
            ))),
        }
    }

    /// True for the two producers that the SQL CHECK allows to leave
    /// `produced_from_version_id` NULL.
    #[must_use]
    pub const fn allows_null_parent(self) -> bool {
        matches!(self, Self::Ingest | Self::ExternalObserved)
    }
}

/// `identity_evidence.target_type` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdentityEvidenceTarget {
    MediaWork,
    MediaVariant,
    AssetBundle,
    FileAsset,
    FileVersion,
    FileLocation,
}

impl IdentityEvidenceTarget {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MediaWork => "media_work",
            Self::MediaVariant => "media_variant",
            Self::AssetBundle => "asset_bundle",
            Self::FileAsset => "file_asset",
            Self::FileVersion => "file_version",
            Self::FileLocation => "file_location",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "media_work" => Ok(Self::MediaWork),
            "media_variant" => Ok(Self::MediaVariant),
            "asset_bundle" => Ok(Self::AssetBundle),
            "file_asset" => Ok(Self::FileAsset),
            "file_version" => Ok(Self::FileVersion),
            "file_location" => Ok(Self::FileLocation),
            other => Err(VoomError::Database(format!(
                "identity_evidence.target_type {other:?} not in vocab"
            ))),
        }
    }
}

// ---------- DiscoveredFile / proof payloads --------------------------------

/// Physical-object proof captured by the watcher at the discovered
/// path. Persisted on the resulting `file_locations` row so future
/// rename reconciliation can prove same-physical-object identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationProof {
    LocalFileIdGeneration {
        file_id: u128,
        generation: u64,
    },
    ObjectStoreVersion {
        bucket: String,
        key: String,
        version_id: String,
    },
}

impl LocationProof {
    pub(crate) fn proof_kind(&self) -> &'static str {
        match self {
            Self::LocalFileIdGeneration { .. } => "file_id_generation",
            Self::ObjectStoreVersion { .. } => "object_version_id",
        }
    }

    pub(crate) fn proof_value(&self) -> String {
        match self {
            Self::LocalFileIdGeneration {
                file_id,
                generation,
            } => serde_json::json!({ "file_id": file_id.to_string(), "generation": generation })
                .to_string(),
            Self::ObjectStoreVersion {
                bucket,
                key,
                version_id,
            } => serde_json::json!({
                "bucket": bucket,
                "key": key,
                "version_id": version_id,
            })
            .to_string(),
        }
    }
}

/// Caller's alias-attach proof. Carries the prior live location's
/// id and the same-physical-object identifier the watcher captured at
/// the new path; the repo cross-checks both against the prior
/// location's stored `(proof_kind, proof_value)` and the
/// `FileVersion`'s hash/size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasProof {
    LocalFileIdGeneration {
        file_id: u128,
        generation: u64,
        prior_location_id: FileLocationId,
    },
    ObjectStoreVersion {
        bucket: String,
        key: String,
        version_id: String,
        prior_location_id: FileLocationId,
    },
}

/// Proof required to reconcile a same-physical-object rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameProof {
    LocalFileIdGeneration {
        prior_location_id: FileLocationId,
        new_kind: FileLocationKind,
        new_value: String,
        file_id: u128,
        generation: u64,
        prior_path_missing: bool,
    },
    ObjectStoreVersion {
        prior_location_id: FileLocationId,
        new_kind: FileLocationKind,
        new_value: String,
        bucket: String,
        key: String,
        version_id: String,
        prior_key_missing: bool,
    },
}

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub location_kind: FileLocationKind,
    pub location_value: String,
    pub content_hash: String,
    pub size_bytes: u64,
    pub observed_at: OffsetDateTime,
    pub proof: Option<LocationProof>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedBytes {
    pub content_hash: String,
    pub size_bytes: u64,
}

/// The three possible outcomes `record_discovered_file_in_tx` surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    NewFileAsset {
        file_asset_id: FileAssetId,
        file_version_id: FileVersionId,
        file_location_id: FileLocationId,
        /// `Some(evidence_id)` when the new bytes' hash matched some
        /// existing `FileVersion`. The repo wrote an
        /// `identity_evidence(hash_match)` row whose `target` is the
        /// *existing* `FileAsset` (not the new one) and whose
        /// `candidate_id` is the new `FileVersion` — per spec §8.7,
        /// hash matches surface the new bytes as a candidate against
        /// the existing logical asset without collapsing identity.
        hash_match_evidence: Option<EvidenceId>,
        /// `Some(evidence_id)` when the caller supplied an
        /// `AliasProof` that failed validation: the repo recorded a
        /// `path_rule_match` evidence row against the new
        /// `FileVersion` alongside the new asset.
        path_rule_evidence: Option<EvidenceId>,
    },
    AliasAttached {
        file_version_id: FileVersionId,
        new_file_location_id: FileLocationId,
    },
}

/// Outcome of `reconcile_rename_in_tx`. Mirrors the
/// `IngestOutcome::RenameReconciled` shape from earlier spec drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameReconciledOutcome {
    pub file_version_id: FileVersionId,
    pub retired_location_id: FileLocationId,
    pub new_file_location_id: FileLocationId,
}

// ---------- row-shape structs ---------------------------------------------

#[derive(Debug, Clone)]
pub struct NewMediaWork {
    pub kind: MediaWorkKind,
    pub display_title: String,
    pub provisional: bool,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct MediaWork {
    pub id: MediaWorkId,
    pub kind: MediaWorkKind,
    pub display_title: String,
    pub provisional: bool,
    pub created_at: OffsetDateTime,
    pub epoch: u64,
}

#[derive(Debug, Clone)]
pub struct NewMediaVariant {
    pub media_work_id: MediaWorkId,
    pub label: String,
    pub provisional: bool,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct MediaVariant {
    pub id: MediaVariantId,
    pub media_work_id: MediaWorkId,
    pub label: String,
    pub provisional: bool,
    pub created_at: OffsetDateTime,
    pub epoch: u64,
}

#[derive(Debug, Clone)]
pub struct FileAsset {
    pub id: FileAssetId,
    pub created_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
    pub epoch: u64,
}

#[derive(Debug, Clone)]
pub struct NewFileVersion {
    pub file_asset_id: FileAssetId,
    pub content_hash: String,
    pub size_bytes: u64,
    pub produced_by: ProducedBy,
    pub produced_from_version_id: Option<FileVersionId>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct FileVersion {
    pub id: FileVersionId,
    pub file_asset_id: FileAssetId,
    pub content_hash: String,
    pub size_bytes: u64,
    pub produced_by: ProducedBy,
    pub produced_from_version_id: Option<FileVersionId>,
    pub created_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFileLocation {
    pub file_version_id: FileVersionId,
    pub kind: FileLocationKind,
    pub value: String,
    pub proof: Option<LocationProof>,
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct FileLocation {
    pub id: FileLocationId,
    pub file_version_id: FileVersionId,
    pub kind: FileLocationKind,
    pub value: String,
    pub proof_kind: Option<String>,
    pub proof_value: Option<String>,
    pub observed_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
    pub epoch: u64,
}

#[derive(Debug, Clone)]
pub struct NewIdentityEvidence {
    pub target_type: IdentityEvidenceTarget,
    pub target_id: u64,
    pub assertion_type: AssertionKind,
    pub candidate_id: Option<u64>,
    pub candidate_value: Option<String>,
    pub provider: String,
    pub provider_version: String,
    pub confidence: f64,
    pub provenance: JsonValue,
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct IdentityEvidence {
    pub id: EvidenceId,
    pub target_type: IdentityEvidenceTarget,
    pub target_id: u64,
    pub assertion_type: AssertionKind,
    pub candidate_id: Option<u64>,
    pub candidate_value: Option<String>,
    pub provider: String,
    pub provider_version: String,
    pub confidence: f64,
    pub provenance: JsonValue,
    pub observed_at: OffsetDateTime,
    pub superseded_at: Option<OffsetDateTime>,
    pub superseded_by_id: Option<EvidenceId>,
    pub accepted_at: Option<OffsetDateTime>,
    pub accepted_user_id: Option<String>,
    pub accepted_policy_id: Option<u64>,
    pub pinned_file_version_ids: Option<JsonValue>,
    pub pinned_hashes: Option<JsonValue>,
    pub pinned_locations: Option<JsonValue>,
}

/// Pinned snapshot stamped onto an `identity_evidence` row at accept-time.
/// Each field is `Option<JsonValue>` so callers may omit one without
/// affecting the others.
#[derive(Debug, Clone, Default)]
pub struct AcceptedPin {
    pub file_version_ids: Option<JsonValue>,
    pub hashes: Option<JsonValue>,
    pub locations: Option<JsonValue>,
}

#[derive(Debug, Clone)]
pub struct NewMediaSnapshot {
    pub file_version_id: FileVersionId,
    pub probed_by: Option<WorkerId>,
    pub probed_at: OffsetDateTime,
    pub payload: JsonValue,
}

#[derive(Debug, Clone)]
pub struct MediaSnapshot {
    pub id: MediaSnapshotId,
    pub file_version_id: FileVersionId,
    pub probed_by: Option<WorkerId>,
    pub probed_at: OffsetDateTime,
    pub payload: JsonValue,
}

// ---------- trait ----------------------------------------------------------

#[async_trait]
pub trait IdentityRepo: Repository {
    // Ingest / rename.
    async fn record_discovered_file_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        discovered: DiscoveredFile,
        alias_proof: Option<AliasProof>,
    ) -> Result<IngestOutcome, VoomError>;
    async fn record_discovered_file(
        &self,
        discovered: DiscoveredFile,
        alias_proof: Option<AliasProof>,
    ) -> Result<IngestOutcome, VoomError>;

    async fn reconcile_rename_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        proof: RenameProof,
        observed: ObservedBytes,
        observed_at: OffsetDateTime,
    ) -> Result<RenameReconciledOutcome, VoomError>;
    async fn reconcile_rename(
        &self,
        proof: RenameProof,
        observed: ObservedBytes,
        observed_at: OffsetDateTime,
    ) -> Result<RenameReconciledOutcome, VoomError>;

    // media_works CRUD.
    async fn create_media_work_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewMediaWork,
    ) -> Result<MediaWork, VoomError>;
    async fn create_media_work(&self, input: NewMediaWork) -> Result<MediaWork, VoomError>;
    async fn get_media_work(&self, id: MediaWorkId) -> Result<Option<MediaWork>, VoomError>;
    async fn list_media_works(&self, limit: u32) -> Result<Vec<MediaWork>, VoomError>;
    async fn update_media_work_provisional_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: MediaWorkId,
        provisional: bool,
        expected_epoch: u64,
    ) -> Result<MediaWork, VoomError>;

    // media_variants CRUD.
    async fn create_media_variant_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewMediaVariant,
    ) -> Result<MediaVariant, VoomError>;
    async fn create_media_variant(&self, input: NewMediaVariant)
    -> Result<MediaVariant, VoomError>;
    async fn get_media_variant(
        &self,
        id: MediaVariantId,
    ) -> Result<Option<MediaVariant>, VoomError>;
    async fn list_media_variants(
        &self,
        media_work_id: MediaWorkId,
    ) -> Result<Vec<MediaVariant>, VoomError>;
    async fn update_media_variant_provisional_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: MediaVariantId,
        provisional: bool,
        expected_epoch: u64,
    ) -> Result<MediaVariant, VoomError>;

    // file_assets CRUD (creation is implicit in record_discovered_file; this
    // is the explicit handle for the rare case where a caller wants to seed
    // an asset directly, e.g. tests).
    async fn create_file_asset_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        created_at: OffsetDateTime,
    ) -> Result<FileAsset, VoomError>;
    async fn create_file_asset(&self, created_at: OffsetDateTime) -> Result<FileAsset, VoomError>;
    async fn get_file_asset(&self, id: FileAssetId) -> Result<Option<FileAsset>, VoomError>;
    async fn retire_file_asset_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileAssetId,
        retired_at: OffsetDateTime,
        expected_epoch: u64,
    ) -> Result<FileAsset, VoomError>;

    // file_versions CRUD.
    async fn create_file_version_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewFileVersion,
    ) -> Result<FileVersion, VoomError>;
    async fn create_file_version(&self, input: NewFileVersion) -> Result<FileVersion, VoomError>;
    async fn get_file_version(&self, id: FileVersionId) -> Result<Option<FileVersion>, VoomError>;
    /// In-tx getter for `file_versions` — required by case-handler
    /// composition so reads inside a transaction see writes the same
    /// transaction made (sqlx-on-SQLite isolates pool reads from an
    /// open tx).
    async fn get_file_version_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileVersionId,
    ) -> Result<Option<FileVersion>, VoomError>;
    async fn list_file_versions_by_asset(
        &self,
        asset_id: FileAssetId,
    ) -> Result<Vec<FileVersion>, VoomError>;
    async fn retire_file_version_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileVersionId,
        retired_at: OffsetDateTime,
        expected_epoch: u64,
    ) -> Result<FileVersion, VoomError>;

    // file_locations CRUD.
    async fn create_file_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewFileLocation,
    ) -> Result<FileLocation, VoomError>;
    async fn get_file_location(
        &self,
        id: FileLocationId,
    ) -> Result<Option<FileLocation>, VoomError>;
    /// In-tx getter — see `get_file_version_in_tx` for rationale.
    async fn get_file_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileLocationId,
    ) -> Result<Option<FileLocation>, VoomError>;
    async fn list_file_locations_by_version(
        &self,
        version_id: FileVersionId,
    ) -> Result<Vec<FileLocation>, VoomError>;
    async fn list_live_file_locations_by_version(
        &self,
        version_id: FileVersionId,
    ) -> Result<Vec<FileLocation>, VoomError>;
    /// In-tx variant of `list_live_file_locations_by_version`.
    /// Required by the commit-safety-gate closure walker (commit 4 /
    /// Phase A): the walker runs inside an IMMEDIATE tx and must see
    /// writes from the same tx, which the pool-reading variant
    /// cannot do (sqlx-on-SQLite isolates pool reads from open
    /// transactions). Returns IDs only — the closure walker folds
    /// them into a `BTreeSet<FileLocationId>` and does not consume
    /// the full row body.
    async fn list_live_file_locations_by_version_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        version_id: FileVersionId,
    ) -> Result<Vec<FileLocationId>, VoomError>;
    async fn retire_file_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileLocationId,
        retired_at: OffsetDateTime,
        expected_epoch: u64,
    ) -> Result<FileLocation, VoomError>;

    // identity_evidence CRUD.
    async fn record_identity_evidence_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewIdentityEvidence,
    ) -> Result<IdentityEvidence, VoomError>;
    async fn get_identity_evidence(
        &self,
        id: EvidenceId,
    ) -> Result<Option<IdentityEvidence>, VoomError>;
    /// In-tx getter — see `get_file_version_in_tx` for rationale.
    async fn get_identity_evidence_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: EvidenceId,
    ) -> Result<Option<IdentityEvidence>, VoomError>;
    /// In-tx list — required by case handlers that need to enumerate
    /// rows the same transaction just wrote (e.g.
    /// `record_discovered_file` or `reconcile_rename` emitting
    /// evidence events).
    async fn list_identity_evidence_by_target_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        target_type: IdentityEvidenceTarget,
        target_id: u64,
    ) -> Result<Vec<IdentityEvidence>, VoomError>;
    async fn list_identity_evidence_by_target(
        &self,
        target_type: IdentityEvidenceTarget,
        target_id: u64,
    ) -> Result<Vec<IdentityEvidence>, VoomError>;
    async fn list_live_identity_evidence_by_target(
        &self,
        target_type: IdentityEvidenceTarget,
        target_id: u64,
    ) -> Result<Vec<IdentityEvidence>, VoomError>;
    async fn accept_identity_evidence_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: EvidenceId,
        actor: Option<String>,
        accepted_at: OffsetDateTime,
        pinned: AcceptedPin,
    ) -> Result<IdentityEvidence, VoomError>;
    async fn supersede_identity_evidence_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        old_id: EvidenceId,
        new_input: NewIdentityEvidence,
        superseded_at: OffsetDateTime,
    ) -> Result<IdentityEvidence, VoomError>;

    // media_snapshots CRUD.
    async fn record_media_snapshot_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewMediaSnapshot,
    ) -> Result<MediaSnapshot, VoomError>;
    async fn get_media_snapshot(
        &self,
        id: MediaSnapshotId,
    ) -> Result<Option<MediaSnapshot>, VoomError>;
    async fn list_media_snapshots_by_version(
        &self,
        version_id: FileVersionId,
    ) -> Result<Vec<MediaSnapshot>, VoomError>;
}

// ---------- SqliteIdentityRepo impl ---------------------------------------

#[derive(Debug, Clone)]
pub struct SqliteIdentityRepo {
    pool: SqlitePool,
}

impl SqliteIdentityRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteIdentityRepo {}

#[async_trait]
impl IdentityRepo for SqliteIdentityRepo {
    async fn record_discovered_file_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        discovered: DiscoveredFile,
        alias_proof: Option<AliasProof>,
    ) -> Result<IngestOutcome, VoomError> {
        match alias_proof {
            None => ingest_new_file_asset(tx, &discovered, None).await,
            Some(proof) => {
                // Validate alias proof against prior live location + version.
                let validation = validate_alias_proof(tx, &discovered, &proof).await?;
                match validation {
                    AliasValidation::Match {
                        file_version_id,
                        prior_location,
                    } => {
                        // Persist the new alias location, requiring its
                        // proof bytes to match the alias_proof bytes
                        // (spec §8.7: "proof drift on alias attach").
                        verify_discovered_proof_matches_alias_proof(&discovered, &proof)?;
                        let new_location_id = insert_file_location(
                            tx,
                            file_version_id,
                            discovered.location_kind,
                            &discovered.location_value,
                            discovered.proof.as_ref(),
                            discovered.observed_at,
                        )
                        .await?;
                        // Touch the prior location's epoch only on
                        // explicit operations; alias-attach leaves the
                        // prior live location intact.
                        let _ = prior_location;
                        Ok(IngestOutcome::AliasAttached {
                            file_version_id,
                            new_file_location_id: new_location_id,
                        })
                    }
                    AliasValidation::Mismatch => {
                        // Caller-supplied alias proof did not match the
                        // prior location / version. Fall through to a
                        // new FileAsset and stamp a path_rule_match
                        // evidence row referencing the new bytes.
                        ingest_new_file_asset(tx, &discovered, Some(())).await
                    }
                }
            }
        }
    }

    async fn record_discovered_file(
        &self,
        discovered: DiscoveredFile,
        alias_proof: Option<AliasProof>,
    ) -> Result<IngestOutcome, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self
            .record_discovered_file_in_tx(&mut tx, discovered, alias_proof)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn reconcile_rename_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        proof: RenameProof,
        observed: ObservedBytes,
        observed_at: OffsetDateTime,
    ) -> Result<RenameReconciledOutcome, VoomError> {
        reconcile_rename_impl(tx, &proof, &observed, observed_at).await
    }

    async fn reconcile_rename(
        &self,
        proof: RenameProof,
        observed: ObservedBytes,
        observed_at: OffsetDateTime,
    ) -> Result<RenameReconciledOutcome, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self
            .reconcile_rename_in_tx(&mut tx, proof, observed, observed_at)
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn create_media_work_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewMediaWork,
    ) -> Result<MediaWork, VoomError> {
        let ts = iso8601(input.created_at)?;
        let res = sqlx::query(
            "INSERT INTO media_works (kind, display_title, provisional, created_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(input.kind.as_str())
        .bind(&input.display_title)
        .bind(i64::from(input.provisional))
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("media_works insert: {e}")))?;
        let id = MediaWorkId(u64_from_i64(res.last_insert_rowid()));
        get_media_work_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("media_works post-insert get vanished: {id}"))
        })
    }

    async fn create_media_work(&self, input: NewMediaWork) -> Result<MediaWork, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.create_media_work_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn get_media_work(&self, id: MediaWorkId) -> Result<Option<MediaWork>, VoomError> {
        let row = sqlx::query(SELECT_MEDIA_WORK_COLS)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("media_works get: {e}")))?;
        row.as_ref().map(row_to_media_work).transpose()
    }

    async fn list_media_works(&self, limit: u32) -> Result<Vec<MediaWork>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, kind, display_title, provisional, created_at, epoch \
             FROM media_works ORDER BY id ASC LIMIT ?",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("media_works list: {e}")))?;
        rows.iter().map(row_to_media_work).collect()
    }

    async fn update_media_work_provisional_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: MediaWorkId,
        provisional: bool,
        expected_epoch: u64,
    ) -> Result<MediaWork, VoomError> {
        let res = sqlx::query(
            "UPDATE media_works SET provisional = ?, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ?",
        )
        .bind(i64::from(provisional))
        .bind(i64_from_u64(id.0))
        .bind(i64_from_u64(expected_epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("media_works update: {e}")))?;
        if res.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "media_works update_provisional: id={id} expected_epoch={expected_epoch} mismatch"
            )));
        }
        get_media_work_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("media_works post-update get vanished: {id}"))
        })
    }

    async fn create_media_variant_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewMediaVariant,
    ) -> Result<MediaVariant, VoomError> {
        let ts = iso8601(input.created_at)?;
        let res = sqlx::query(
            "INSERT INTO media_variants (media_work_id, label, provisional, created_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.media_work_id.0))
        .bind(&input.label)
        .bind(i64::from(input.provisional))
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("media_variants insert: {e}")))?;
        let id = MediaVariantId(u64_from_i64(res.last_insert_rowid()));
        get_media_variant_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("media_variants post-insert get vanished: {id}"))
        })
    }

    async fn create_media_variant(
        &self,
        input: NewMediaVariant,
    ) -> Result<MediaVariant, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.create_media_variant_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn get_media_variant(
        &self,
        id: MediaVariantId,
    ) -> Result<Option<MediaVariant>, VoomError> {
        let row = sqlx::query(SELECT_MEDIA_VARIANT_COLS)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("media_variants get: {e}")))?;
        row.as_ref().map(row_to_media_variant).transpose()
    }

    async fn list_media_variants(
        &self,
        media_work_id: MediaWorkId,
    ) -> Result<Vec<MediaVariant>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, media_work_id, label, provisional, created_at, epoch \
             FROM media_variants WHERE media_work_id = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(media_work_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("media_variants list: {e}")))?;
        rows.iter().map(row_to_media_variant).collect()
    }

    async fn update_media_variant_provisional_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: MediaVariantId,
        provisional: bool,
        expected_epoch: u64,
    ) -> Result<MediaVariant, VoomError> {
        let res = sqlx::query(
            "UPDATE media_variants SET provisional = ?, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ?",
        )
        .bind(i64::from(provisional))
        .bind(i64_from_u64(id.0))
        .bind(i64_from_u64(expected_epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("media_variants update: {e}")))?;
        if res.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "media_variants update_provisional: id={id} expected_epoch={expected_epoch} mismatch"
            )));
        }
        get_media_variant_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("media_variants post-update get vanished: {id}"))
        })
    }

    async fn create_file_asset_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        created_at: OffsetDateTime,
    ) -> Result<FileAsset, VoomError> {
        let ts = iso8601(created_at)?;
        let res = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
            .bind(&ts)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("file_assets insert: {e}")))?;
        let id = FileAssetId(u64_from_i64(res.last_insert_rowid()));
        get_file_asset_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("file_assets post-insert get vanished: {id}"))
        })
    }

    async fn create_file_asset(&self, created_at: OffsetDateTime) -> Result<FileAsset, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.create_file_asset_in_tx(&mut tx, created_at).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn get_file_asset(&self, id: FileAssetId) -> Result<Option<FileAsset>, VoomError> {
        let row = sqlx::query(SELECT_FILE_ASSET_COLS)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("file_assets get: {e}")))?;
        row.as_ref().map(row_to_file_asset).transpose()
    }

    async fn retire_file_asset_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileAssetId,
        retired_at: OffsetDateTime,
        expected_epoch: u64,
    ) -> Result<FileAsset, VoomError> {
        let ts = iso8601(retired_at)?;
        let res = sqlx::query(
            "UPDATE file_assets SET retired_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ? AND retired_at IS NULL",
        )
        .bind(&ts)
        .bind(i64_from_u64(id.0))
        .bind(i64_from_u64(expected_epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("file_assets retire: {e}")))?;
        if res.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "file_assets retire: id={id} expected_epoch={expected_epoch} or already retired"
            )));
        }
        get_file_asset_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("file_assets post-retire get vanished: {id}"))
        })
    }

    async fn create_file_version_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewFileVersion,
    ) -> Result<FileVersion, VoomError> {
        // Repo-side lineage check: non-(ingest|external_observed) producers
        // require a parent; if a parent is given, validate it belongs to
        // the same FileAsset and matches the produced_from contract.
        if !input.produced_by.allows_null_parent() && input.produced_from_version_id.is_none() {
            return Err(VoomError::Conflict(format!(
                "file_versions: produced_by={} requires produced_from_version_id",
                input.produced_by.as_str()
            )));
        }
        if let Some(parent_id) = input.produced_from_version_id {
            let parent_asset: Option<i64> =
                sqlx::query_scalar("SELECT file_asset_id FROM file_versions WHERE id = ?")
                    .bind(i64_from_u64(parent_id.0))
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(|e| {
                        VoomError::Database(format!("file_versions parent lookup: {e}"))
                    })?;
            let parent_asset = parent_asset.ok_or_else(|| {
                VoomError::NotFound(format!("file_versions parent {parent_id} missing"))
            })?;
            if u64_from_i64(parent_asset) != input.file_asset_id.0 {
                return Err(VoomError::Conflict(format!(
                    "file_versions: parent {parent_id} belongs to a different file_asset"
                )));
            }
        }
        let ts = iso8601(input.created_at)?;
        let size_i64 = i64::try_from(input.size_bytes).map_err(|_| {
            VoomError::Config(format!(
                "file_versions: size_bytes {} overflows i64",
                input.size_bytes
            ))
        })?;
        let res = sqlx::query(
            "INSERT INTO file_versions \
             (file_asset_id, content_hash, size_bytes, produced_by, \
              produced_from_version_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.file_asset_id.0))
        .bind(&input.content_hash)
        .bind(size_i64)
        .bind(input.produced_by.as_str())
        .bind(input.produced_from_version_id.map(|v| i64_from_u64(v.0)))
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("file_versions insert: {e}")))?;
        let id = FileVersionId(u64_from_i64(res.last_insert_rowid()));
        get_file_version_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("file_versions post-insert get vanished: {id}"))
        })
    }

    async fn create_file_version(&self, input: NewFileVersion) -> Result<FileVersion, VoomError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let out = self.create_file_version_in_tx(&mut tx, input).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(out)
    }

    async fn get_file_version(&self, id: FileVersionId) -> Result<Option<FileVersion>, VoomError> {
        let row = sqlx::query(SELECT_FILE_VERSION_COLS)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("file_versions get: {e}")))?;
        row.as_ref().map(row_to_file_version).transpose()
    }

    async fn get_file_version_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileVersionId,
    ) -> Result<Option<FileVersion>, VoomError> {
        get_file_version_in_tx(tx, id).await
    }

    async fn list_file_versions_by_asset(
        &self,
        asset_id: FileAssetId,
    ) -> Result<Vec<FileVersion>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, file_asset_id, content_hash, size_bytes, produced_by, \
                    produced_from_version_id, created_at, retired_at, epoch \
             FROM file_versions WHERE file_asset_id = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(asset_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("file_versions list: {e}")))?;
        rows.iter().map(row_to_file_version).collect()
    }

    async fn retire_file_version_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileVersionId,
        retired_at: OffsetDateTime,
        expected_epoch: u64,
    ) -> Result<FileVersion, VoomError> {
        let ts = iso8601(retired_at)?;
        let res = sqlx::query(
            "UPDATE file_versions SET retired_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ? AND retired_at IS NULL",
        )
        .bind(&ts)
        .bind(i64_from_u64(id.0))
        .bind(i64_from_u64(expected_epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("file_versions retire: {e}")))?;
        if res.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "file_versions retire: id={id} expected_epoch={expected_epoch} or already retired"
            )));
        }
        get_file_version_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("file_versions post-retire get vanished: {id}"))
        })
    }

    async fn create_file_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewFileLocation,
    ) -> Result<FileLocation, VoomError> {
        let id = insert_file_location(
            tx,
            input.file_version_id,
            input.kind,
            &input.value,
            input.proof.as_ref(),
            input.observed_at,
        )
        .await?;
        get_file_location_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("file_locations post-insert get vanished: {id}"))
        })
    }

    async fn get_file_location(
        &self,
        id: FileLocationId,
    ) -> Result<Option<FileLocation>, VoomError> {
        let row = sqlx::query(SELECT_FILE_LOCATION_COLS)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("file_locations get: {e}")))?;
        row.as_ref().map(row_to_file_location).transpose()
    }

    async fn get_file_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileLocationId,
    ) -> Result<Option<FileLocation>, VoomError> {
        get_file_location_in_tx(tx, id).await
    }

    async fn list_file_locations_by_version(
        &self,
        version_id: FileVersionId,
    ) -> Result<Vec<FileLocation>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, file_version_id, kind, value, proof_kind, proof_value, \
                    observed_at, retired_at, epoch \
             FROM file_locations WHERE file_version_id = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(version_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("file_locations list: {e}")))?;
        rows.iter().map(row_to_file_location).collect()
    }

    async fn list_live_file_locations_by_version(
        &self,
        version_id: FileVersionId,
    ) -> Result<Vec<FileLocation>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, file_version_id, kind, value, proof_kind, proof_value, \
                    observed_at, retired_at, epoch \
             FROM file_locations WHERE file_version_id = ? AND retired_at IS NULL \
             ORDER BY id ASC",
        )
        .bind(i64_from_u64(version_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("file_locations list live: {e}")))?;
        rows.iter().map(row_to_file_location).collect()
    }

    async fn list_live_file_locations_by_version_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        version_id: FileVersionId,
    ) -> Result<Vec<FileLocationId>, VoomError> {
        let rows: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM file_locations \
             WHERE file_version_id = ? AND retired_at IS NULL \
             ORDER BY id ASC",
        )
        .bind(i64_from_u64(version_id.0))
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("file_locations list live in_tx: {e}")))?;
        rows.into_iter()
            .map(|id| {
                u64::try_from(id)
                    .map(FileLocationId)
                    .map_err(|e| VoomError::Internal(format!("file_locations id signedness: {e}")))
            })
            .collect()
    }

    async fn retire_file_location_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: FileLocationId,
        retired_at: OffsetDateTime,
        expected_epoch: u64,
    ) -> Result<FileLocation, VoomError> {
        let ts = iso8601(retired_at)?;
        let res = sqlx::query(
            "UPDATE file_locations SET retired_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ? AND retired_at IS NULL",
        )
        .bind(&ts)
        .bind(i64_from_u64(id.0))
        .bind(i64_from_u64(expected_epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("file_locations retire: {e}")))?;
        if res.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "file_locations retire: id={id} expected_epoch={expected_epoch} or already retired"
            )));
        }
        get_file_location_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("file_locations post-retire get vanished: {id}"))
        })
    }

    async fn record_identity_evidence_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewIdentityEvidence,
    ) -> Result<IdentityEvidence, VoomError> {
        let id = insert_identity_evidence(tx, &input).await?;
        get_identity_evidence_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("identity_evidence post-insert get vanished: {id}"))
        })
    }

    async fn get_identity_evidence(
        &self,
        id: EvidenceId,
    ) -> Result<Option<IdentityEvidence>, VoomError> {
        let row = sqlx::query(SELECT_IDENTITY_EVIDENCE_COLS)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("identity_evidence get: {e}")))?;
        row.as_ref().map(row_to_identity_evidence).transpose()
    }

    async fn get_identity_evidence_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: EvidenceId,
    ) -> Result<Option<IdentityEvidence>, VoomError> {
        get_identity_evidence_in_tx(tx, id).await
    }

    async fn list_identity_evidence_by_target_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        target_type: IdentityEvidenceTarget,
        target_id: u64,
    ) -> Result<Vec<IdentityEvidence>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, target_type, target_id, assertion_type, candidate_id, candidate_value, \
                    provider, provider_version, confidence, provenance, observed_at, \
                    superseded_at, superseded_by_id, accepted_at, accepted_user_id, \
                    accepted_policy_id, pinned_file_version_ids, pinned_hashes, pinned_locations \
             FROM identity_evidence WHERE target_type = ? AND target_id = ? ORDER BY id ASC",
        )
        .bind(target_type.as_str())
        .bind(i64_from_u64(target_id))
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("identity_evidence list_in_tx: {e}")))?;
        rows.iter().map(row_to_identity_evidence).collect()
    }

    async fn list_identity_evidence_by_target(
        &self,
        target_type: IdentityEvidenceTarget,
        target_id: u64,
    ) -> Result<Vec<IdentityEvidence>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, target_type, target_id, assertion_type, candidate_id, candidate_value, \
                    provider, provider_version, confidence, provenance, observed_at, \
                    superseded_at, superseded_by_id, accepted_at, accepted_user_id, \
                    accepted_policy_id, pinned_file_version_ids, pinned_hashes, pinned_locations \
             FROM identity_evidence WHERE target_type = ? AND target_id = ? ORDER BY id ASC",
        )
        .bind(target_type.as_str())
        .bind(i64_from_u64(target_id))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("identity_evidence list: {e}")))?;
        rows.iter().map(row_to_identity_evidence).collect()
    }

    async fn list_live_identity_evidence_by_target(
        &self,
        target_type: IdentityEvidenceTarget,
        target_id: u64,
    ) -> Result<Vec<IdentityEvidence>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, target_type, target_id, assertion_type, candidate_id, candidate_value, \
                    provider, provider_version, confidence, provenance, observed_at, \
                    superseded_at, superseded_by_id, accepted_at, accepted_user_id, \
                    accepted_policy_id, pinned_file_version_ids, pinned_hashes, pinned_locations \
             FROM identity_evidence WHERE target_type = ? AND target_id = ? \
                                       AND superseded_at IS NULL ORDER BY id ASC",
        )
        .bind(target_type.as_str())
        .bind(i64_from_u64(target_id))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("identity_evidence list live: {e}")))?;
        rows.iter().map(row_to_identity_evidence).collect()
    }

    async fn accept_identity_evidence_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        id: EvidenceId,
        actor: Option<String>,
        accepted_at: OffsetDateTime,
        pinned: AcceptedPin,
    ) -> Result<IdentityEvidence, VoomError> {
        let ts = iso8601(accepted_at)?;
        // Pre-validate pinned JSON shape so we surface a typed error
        // rather than letting the SQL CHECK trip.
        let pinned_fv = pinned
            .file_version_ids
            .as_ref()
            .map(|v| serialize_json(v, "pinned_file_version_ids"))
            .transpose()?;
        let pinned_hashes = pinned
            .hashes
            .as_ref()
            .map(|v| serialize_json(v, "pinned_hashes"))
            .transpose()?;
        let pinned_locations = pinned
            .locations
            .as_ref()
            .map(|v| serialize_json(v, "pinned_locations"))
            .transpose()?;
        let res = sqlx::query(
            "UPDATE identity_evidence SET accepted_at = ?, accepted_user_id = ?, \
                 pinned_file_version_ids = ?, pinned_hashes = ?, pinned_locations = ? \
             WHERE id = ? AND accepted_at IS NULL AND superseded_at IS NULL",
        )
        .bind(&ts)
        .bind(&actor)
        .bind(pinned_fv)
        .bind(pinned_hashes)
        .bind(pinned_locations)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("identity_evidence accept: {e}")))?;
        if res.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "identity_evidence accept: id={id} already accepted, superseded, or missing"
            )));
        }
        get_identity_evidence_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("identity_evidence post-accept get vanished: {id}"))
        })
    }

    async fn supersede_identity_evidence_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        old_id: EvidenceId,
        new_input: NewIdentityEvidence,
        superseded_at: OffsetDateTime,
    ) -> Result<IdentityEvidence, VoomError> {
        // 1. Insert the new evidence row (it carries observed_at and the
        //    full assertion payload).
        let new_id = insert_identity_evidence(tx, &new_input).await?;
        // 2. Mark the old row superseded, pointing at the new id.
        let ts = iso8601(superseded_at)?;
        let res = sqlx::query(
            "UPDATE identity_evidence SET superseded_at = ?, superseded_by_id = ? \
             WHERE id = ? AND superseded_at IS NULL",
        )
        .bind(&ts)
        .bind(i64_from_u64(new_id.0))
        .bind(i64_from_u64(old_id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("identity_evidence supersede: {e}")))?;
        if res.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "identity_evidence supersede: id={old_id} already superseded or missing"
            )));
        }
        get_identity_evidence_in_tx(tx, new_id)
            .await?
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "identity_evidence post-supersede get vanished: {new_id}"
                ))
            })
    }

    async fn record_media_snapshot_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewMediaSnapshot,
    ) -> Result<MediaSnapshot, VoomError> {
        let payload_str = serialize_json(&input.payload, "media_snapshots.payload")?;
        let ts = iso8601(input.probed_at)?;
        let res = sqlx::query(
            "INSERT INTO media_snapshots (file_version_id, probed_by, probed_at, payload) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(i64_from_u64(input.file_version_id.0))
        .bind(input.probed_by.map(|w| i64_from_u64(w.0)))
        .bind(&ts)
        .bind(payload_str)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("media_snapshots insert: {e}")))?;
        let id = MediaSnapshotId(u64_from_i64(res.last_insert_rowid()));
        get_media_snapshot_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("media_snapshots post-insert get vanished: {id}"))
        })
    }

    async fn get_media_snapshot(
        &self,
        id: MediaSnapshotId,
    ) -> Result<Option<MediaSnapshot>, VoomError> {
        let row = sqlx::query(SELECT_MEDIA_SNAPSHOT_COLS)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("media_snapshots get: {e}")))?;
        row.as_ref().map(row_to_media_snapshot).transpose()
    }

    async fn list_media_snapshots_by_version(
        &self,
        version_id: FileVersionId,
    ) -> Result<Vec<MediaSnapshot>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, file_version_id, probed_by, probed_at, payload \
             FROM media_snapshots WHERE file_version_id = ? ORDER BY id ASC",
        )
        .bind(i64_from_u64(version_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("media_snapshots list: {e}")))?;
        rows.iter().map(row_to_media_snapshot).collect()
    }
}

// ---------- inner helpers --------------------------------------------------

/// Outcome of validating an alias proof against the prior live
/// location / version. Either everything lines up (caller proceeds to
/// `AliasAttached`) or any field disagrees (caller falls back to
/// `NewFileAsset` + `path_rule_match` evidence).
enum AliasValidation {
    Match {
        file_version_id: FileVersionId,
        prior_location: FileLocation,
    },
    Mismatch,
}

async fn validate_alias_proof(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    discovered: &DiscoveredFile,
    proof: &AliasProof,
) -> Result<AliasValidation, VoomError> {
    let (prior_location_id, expected_kind, expected_value) = match proof {
        AliasProof::LocalFileIdGeneration {
            file_id,
            generation,
            prior_location_id,
        } => (
            *prior_location_id,
            "file_id_generation",
            serde_json::json!({ "file_id": file_id.to_string(), "generation": generation })
                .to_string(),
        ),
        AliasProof::ObjectStoreVersion {
            bucket,
            key,
            version_id,
            prior_location_id,
        } => (
            *prior_location_id,
            "object_version_id",
            serde_json::json!({
                "bucket": bucket,
                "key": key,
                "version_id": version_id,
            })
            .to_string(),
        ),
    };
    let Some(prior) = get_file_location_in_tx(tx, prior_location_id).await? else {
        return Ok(AliasValidation::Mismatch);
    };
    if prior.retired_at.is_some() {
        return Ok(AliasValidation::Mismatch);
    }
    if prior.proof_kind.as_deref() != Some(expected_kind) {
        return Ok(AliasValidation::Mismatch);
    }
    if prior.proof_value.as_deref() != Some(expected_value.as_str()) {
        return Ok(AliasValidation::Mismatch);
    }
    let Some(version) = get_file_version_in_tx(tx, prior.file_version_id).await? else {
        return Ok(AliasValidation::Mismatch);
    };
    if version.content_hash != discovered.content_hash
        || version.size_bytes != discovered.size_bytes
    {
        return Ok(AliasValidation::Mismatch);
    }
    Ok(AliasValidation::Match {
        file_version_id: version.id,
        prior_location: prior,
    })
}

fn verify_discovered_proof_matches_alias_proof(
    discovered: &DiscoveredFile,
    alias_proof: &AliasProof,
) -> Result<(), VoomError> {
    let Some(found) = discovered.proof.as_ref() else {
        return Err(VoomError::Conflict(
            "proof drift on alias attach: discovered.proof is None but alias_proof set".to_owned(),
        ));
    };
    let drift = match (found, alias_proof) {
        (
            LocationProof::LocalFileIdGeneration {
                file_id: fid_a,
                generation: gen_a,
            },
            AliasProof::LocalFileIdGeneration {
                file_id: fid_b,
                generation: gen_b,
                ..
            },
        ) => fid_a != fid_b || gen_a != gen_b,
        (
            LocationProof::ObjectStoreVersion {
                bucket: b_a,
                key: k_a,
                version_id: v_a,
            },
            AliasProof::ObjectStoreVersion {
                bucket: b_b,
                key: k_b,
                version_id: v_b,
                ..
            },
        ) => b_a != b_b || k_a != k_b || v_a != v_b,
        _ => true, // variant disagreement
    };
    if drift {
        return Err(VoomError::Conflict(
            "proof drift on alias attach".to_owned(),
        ));
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the four-row insert + two evidence-stamp branches are best read inline; \
              splitting would shred the spec §8.7 NewFileAsset semantics across helpers"
)]
async fn ingest_new_file_asset(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    discovered: &DiscoveredFile,
    fallback_from_alias_mismatch: Option<()>,
) -> Result<IngestOutcome, VoomError> {
    // Insert FileAsset.
    let ts = iso8601(discovered.observed_at)?;
    let asset_res = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("file_assets insert: {e}")))?;
    let asset_id = FileAssetId(u64_from_i64(asset_res.last_insert_rowid()));

    // Insert FileVersion (produced_by = 'ingest', parent NULL).
    let size_i64 = i64::try_from(discovered.size_bytes).map_err(|_| {
        VoomError::Config(format!(
            "file_versions: size_bytes {} overflows i64",
            discovered.size_bytes
        ))
    })?;
    let version_res = sqlx::query(
        "INSERT INTO file_versions \
         (file_asset_id, content_hash, size_bytes, produced_by, \
          produced_from_version_id, created_at) \
         VALUES (?, ?, ?, 'ingest', NULL, ?)",
    )
    .bind(i64_from_u64(asset_id.0))
    .bind(&discovered.content_hash)
    .bind(size_i64)
    .bind(&ts)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("file_versions insert: {e}")))?;
    let version_id = FileVersionId(u64_from_i64(version_res.last_insert_rowid()));

    // Insert FileLocation carrying the discovered proof (if any).
    let location_id = insert_file_location(
        tx,
        version_id,
        discovered.location_kind,
        &discovered.location_value,
        discovered.proof.as_ref(),
        discovered.observed_at,
    )
    .await?;

    // Hash-match evidence: if any pre-existing FileVersion (on a
    // different FileAsset — we just inserted version_id into asset_id)
    // shares the content hash, stamp a `hash_match` row.
    //
    // Per spec §8.7 the row's target is the *existing* `FileAsset`
    // (so the existing logical asset accumulates candidates) and the
    // candidate is the *new* `FileVersion` (the bytes that just
    // arrived). Hash matches never collapse identity — they surface
    // the candidate without merging assets.
    let prior: Option<(i64, i64)> = sqlx::query_as(
        "SELECT id, file_asset_id FROM file_versions \
         WHERE content_hash = ? AND id <> ? LIMIT 1",
    )
    .bind(&discovered.content_hash)
    .bind(i64_from_u64(version_id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("file_versions hash-match probe: {e}")))?;
    let hash_match_evidence = if let Some((prior_version_i, prior_asset_i)) = prior {
        let prior_version_id = FileVersionId(u64_from_i64(prior_version_i));
        let prior_asset_id = FileAssetId(u64_from_i64(prior_asset_i));
        let ev = insert_identity_evidence(
            tx,
            &NewIdentityEvidence {
                target_type: IdentityEvidenceTarget::FileAsset,
                target_id: prior_asset_id.0,
                assertion_type: AssertionKind::HashMatch,
                candidate_id: Some(version_id.0),
                candidate_value: None,
                provider: "voom.ingest".to_owned(),
                provider_version: "1".to_owned(),
                confidence: 1.0,
                provenance: serde_json::json!({
                    "discovered_path": discovered.location_value,
                    "new_file_version_id": version_id.0,
                    "matched_prior_file_version_id": prior_version_id.0,
                }),
                observed_at: discovered.observed_at,
            },
        )
        .await?;
        Some(ev)
    } else {
        None
    };

    // Path-rule evidence: only when the caller supplied an alias proof
    // that we then rejected (so we want to surface the near-miss for
    // operators / future reconciliation).
    let path_rule_evidence = if fallback_from_alias_mismatch.is_some() {
        Some(
            insert_identity_evidence(
                tx,
                &NewIdentityEvidence {
                    target_type: IdentityEvidenceTarget::FileVersion,
                    target_id: version_id.0,
                    assertion_type: AssertionKind::PathRuleMatch,
                    candidate_id: None,
                    candidate_value: Some(discovered.location_value.clone()),
                    provider: "voom.ingest".to_owned(),
                    provider_version: "1".to_owned(),
                    confidence: 0.5,
                    provenance: serde_json::json!({
                        "discovered_path": discovered.location_value,
                        "alias_proof_validation": "mismatch",
                    }),
                    observed_at: discovered.observed_at,
                },
            )
            .await?,
        )
    } else {
        None
    };

    Ok(IngestOutcome::NewFileAsset {
        file_asset_id: asset_id,
        file_version_id: version_id,
        file_location_id: location_id,
        hash_match_evidence,
        path_rule_evidence,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "the eight-step validation pipeline in spec §8.7 reads cleanly inline; \
              factoring it would scatter the same-physical-object invariants across helpers"
)]
async fn reconcile_rename_impl(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    proof: &RenameProof,
    observed: &ObservedBytes,
    observed_at: OffsetDateTime,
) -> Result<RenameReconciledOutcome, VoomError> {
    let (
        prior_location_id,
        new_kind,
        new_value,
        expected_proof_kind,
        expected_proof_value,
        prior_observed_present,
    ) = match proof {
        RenameProof::LocalFileIdGeneration {
            prior_location_id,
            new_kind,
            new_value,
            file_id,
            generation,
            prior_path_missing,
        } => (
            *prior_location_id,
            *new_kind,
            new_value.clone(),
            "file_id_generation",
            serde_json::json!({
                "file_id": file_id.to_string(),
                "generation": generation,
            })
            .to_string(),
            *prior_path_missing,
        ),
        RenameProof::ObjectStoreVersion {
            prior_location_id,
            new_kind,
            new_value,
            bucket,
            key,
            version_id,
            prior_key_missing,
        } => (
            *prior_location_id,
            *new_kind,
            new_value.clone(),
            "object_version_id",
            serde_json::json!({
                "bucket": bucket,
                "key": key,
                "version_id": version_id,
            })
            .to_string(),
            *prior_key_missing,
        ),
    };

    // 1. Load prior location, require live.
    let prior = get_file_location_in_tx(tx, prior_location_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("file_locations {prior_location_id}")))?;
    if prior.retired_at.is_some() {
        return Err(VoomError::Conflict(format!(
            "reconcile_rename: prior location {prior_location_id} already retired"
        )));
    }
    // 2. proof_kind match.
    if prior.proof_kind.as_deref() != Some(expected_proof_kind) {
        return Err(VoomError::Conflict(format!(
            "reconcile_rename: proof_kind mismatch on {prior_location_id}"
        )));
    }
    // 3. proof_value match.
    if prior.proof_value.as_deref() != Some(expected_proof_value.as_str()) {
        return Err(VoomError::Conflict(format!(
            "reconcile_rename: proof_value mismatch on {prior_location_id}"
        )));
    }
    // 4. Require caller observed prior path missing.
    if !prior_observed_present {
        return Err(VoomError::Conflict(
            "reconcile_rename: rename requires prior path missing".to_owned(),
        ));
    }
    // 5. Hash + size must match the bound FileVersion.
    let version = get_file_version_in_tx(tx, prior.file_version_id)
        .await?
        .ok_or_else(|| {
            VoomError::Internal(format!(
                "reconcile_rename: file_version {} vanished",
                prior.file_version_id
            ))
        })?;
    if version.content_hash != observed.content_hash {
        return Err(VoomError::Conflict(
            "reconcile_rename: hash drift during rename".to_owned(),
        ));
    }
    if version.size_bytes != observed.size_bytes {
        return Err(VoomError::Conflict(
            "reconcile_rename: size drift during rename".to_owned(),
        ));
    }
    // 6. Retire the prior location.
    let ts = iso8601(observed_at)?;
    let retire_res = sqlx::query(
        "UPDATE file_locations SET retired_at = ?, epoch = epoch + 1 \
         WHERE id = ? AND retired_at IS NULL",
    )
    .bind(&ts)
    .bind(i64_from_u64(prior_location_id.0))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("reconcile_rename retire: {e}")))?;
    if retire_res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "reconcile_rename: race on retire of {prior_location_id}"
        )));
    }
    // 7. Insert new location with the same proof bytes carried over.
    let new_location_id = insert_file_location_with_raw_proof(
        tx,
        version.id,
        new_kind,
        &new_value,
        Some(expected_proof_kind),
        Some(expected_proof_value.as_str()),
        observed_at,
    )
    .await?;
    // 9. Append path_rule_match evidence observing the new location.
    insert_identity_evidence(
        tx,
        &NewIdentityEvidence {
            target_type: IdentityEvidenceTarget::FileLocation,
            target_id: new_location_id.0,
            assertion_type: AssertionKind::PathRuleMatch,
            candidate_id: Some(prior_location_id.0),
            candidate_value: Some(new_value.clone()),
            provider: "voom.rename_reconcile".to_owned(),
            provider_version: "1".to_owned(),
            confidence: 1.0,
            provenance: serde_json::json!({
                "prior_location_id": prior_location_id.0,
                "new_location_id": new_location_id.0,
            }),
            observed_at,
        },
    )
    .await?;
    Ok(RenameReconciledOutcome {
        file_version_id: version.id,
        retired_location_id: prior_location_id,
        new_file_location_id: new_location_id,
    })
}

async fn insert_file_location(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    file_version_id: FileVersionId,
    kind: FileLocationKind,
    value: &str,
    proof: Option<&LocationProof>,
    observed_at: OffsetDateTime,
) -> Result<FileLocationId, VoomError> {
    let (proof_kind, proof_value) = match proof {
        None => (None, None),
        Some(p) => (Some(p.proof_kind()), Some(p.proof_value())),
    };
    insert_file_location_with_raw_proof(
        tx,
        file_version_id,
        kind,
        value,
        proof_kind,
        proof_value.as_deref(),
        observed_at,
    )
    .await
}

async fn insert_file_location_with_raw_proof(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    file_version_id: FileVersionId,
    kind: FileLocationKind,
    value: &str,
    proof_kind: Option<&str>,
    proof_value: Option<&str>,
    observed_at: OffsetDateTime,
) -> Result<FileLocationId, VoomError> {
    let ts = iso8601(observed_at)?;
    let res = sqlx::query(
        "INSERT INTO file_locations \
         (file_version_id, kind, value, proof_kind, proof_value, observed_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(i64_from_u64(file_version_id.0))
    .bind(kind.as_str())
    .bind(value)
    .bind(proof_kind)
    .bind(proof_value)
    .bind(&ts)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("file_locations insert: {e}")))?;
    Ok(FileLocationId(u64_from_i64(res.last_insert_rowid())))
}

async fn insert_identity_evidence(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &NewIdentityEvidence,
) -> Result<EvidenceId, VoomError> {
    let provenance_str = serialize_json(&input.provenance, "identity_evidence.provenance")?;
    let ts = iso8601(input.observed_at)?;
    let res = sqlx::query(
        "INSERT INTO identity_evidence \
         (target_type, target_id, assertion_type, candidate_id, candidate_value, \
          provider, provider_version, confidence, provenance, observed_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(input.target_type.as_str())
    .bind(i64_from_u64(input.target_id))
    .bind(input.assertion_type.as_str())
    .bind(input.candidate_id.map(i64_from_u64))
    .bind(&input.candidate_value)
    .bind(&input.provider)
    .bind(&input.provider_version)
    .bind(input.confidence)
    .bind(provenance_str)
    .bind(&ts)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("identity_evidence insert: {e}")))?;
    Ok(EvidenceId(u64_from_i64(res.last_insert_rowid())))
}

// ---------- _in_tx getters and row decoders -------------------------------

const SELECT_MEDIA_WORK_COLS: &str = "SELECT id, kind, display_title, provisional, created_at, epoch \
     FROM media_works WHERE id = ?";

async fn get_media_work_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: MediaWorkId,
) -> Result<Option<MediaWork>, VoomError> {
    let row = sqlx::query(SELECT_MEDIA_WORK_COLS)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("media_works get_in_tx: {e}")))?;
    row.as_ref().map(row_to_media_work).transpose()
}

fn row_to_media_work(row: &sqlx::sqlite::SqliteRow) -> Result<MediaWork, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("media_works", &e))?;
    let kind: String = row
        .try_get("kind")
        .map_err(|e| map_row_err("media_works", &e))?;
    let display_title: String = row
        .try_get("display_title")
        .map_err(|e| map_row_err("media_works", &e))?;
    let provisional: i64 = row
        .try_get("provisional")
        .map_err(|e| map_row_err("media_works", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("media_works", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("media_works", &e))?;
    Ok(MediaWork {
        id: MediaWorkId(u64_from_i64(id)),
        kind: MediaWorkKind::parse(&kind)?,
        display_title,
        provisional: provisional != 0,
        created_at: parse_iso8601(&created_at)?,
        epoch: u64_from_i64(epoch),
    })
}

const SELECT_MEDIA_VARIANT_COLS: &str = "SELECT id, media_work_id, label, provisional, created_at, epoch \
     FROM media_variants WHERE id = ?";

async fn get_media_variant_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: MediaVariantId,
) -> Result<Option<MediaVariant>, VoomError> {
    let row = sqlx::query(SELECT_MEDIA_VARIANT_COLS)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("media_variants get_in_tx: {e}")))?;
    row.as_ref().map(row_to_media_variant).transpose()
}

fn row_to_media_variant(row: &sqlx::sqlite::SqliteRow) -> Result<MediaVariant, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("media_variants", &e))?;
    let media_work_id: i64 = row
        .try_get("media_work_id")
        .map_err(|e| map_row_err("media_variants", &e))?;
    let label: String = row
        .try_get("label")
        .map_err(|e| map_row_err("media_variants", &e))?;
    let provisional: i64 = row
        .try_get("provisional")
        .map_err(|e| map_row_err("media_variants", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("media_variants", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("media_variants", &e))?;
    Ok(MediaVariant {
        id: MediaVariantId(u64_from_i64(id)),
        media_work_id: MediaWorkId(u64_from_i64(media_work_id)),
        label,
        provisional: provisional != 0,
        created_at: parse_iso8601(&created_at)?,
        epoch: u64_from_i64(epoch),
    })
}

const SELECT_FILE_ASSET_COLS: &str =
    "SELECT id, created_at, retired_at, epoch FROM file_assets WHERE id = ?";

async fn get_file_asset_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileAssetId,
) -> Result<Option<FileAsset>, VoomError> {
    let row = sqlx::query(SELECT_FILE_ASSET_COLS)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("file_assets get_in_tx: {e}")))?;
    row.as_ref().map(row_to_file_asset).transpose()
}

fn row_to_file_asset(row: &sqlx::sqlite::SqliteRow) -> Result<FileAsset, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("file_assets", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("file_assets", &e))?;
    let retired_at: Option<String> = row
        .try_get("retired_at")
        .map_err(|e| map_row_err("file_assets", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("file_assets", &e))?;
    Ok(FileAsset {
        id: FileAssetId(u64_from_i64(id)),
        created_at: parse_iso8601(&created_at)?,
        retired_at: retired_at.map(|s| parse_iso8601(&s)).transpose()?,
        epoch: u64_from_i64(epoch),
    })
}

const SELECT_FILE_VERSION_COLS: &str = "SELECT id, file_asset_id, content_hash, size_bytes, produced_by, \
            produced_from_version_id, created_at, retired_at, epoch \
     FROM file_versions WHERE id = ?";

async fn get_file_version_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileVersionId,
) -> Result<Option<FileVersion>, VoomError> {
    let row = sqlx::query(SELECT_FILE_VERSION_COLS)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("file_versions get_in_tx: {e}")))?;
    row.as_ref().map(row_to_file_version).transpose()
}

fn row_to_file_version(row: &sqlx::sqlite::SqliteRow) -> Result<FileVersion, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("file_versions", &e))?;
    let file_asset_id: i64 = row
        .try_get("file_asset_id")
        .map_err(|e| map_row_err("file_versions", &e))?;
    let content_hash: String = row
        .try_get("content_hash")
        .map_err(|e| map_row_err("file_versions", &e))?;
    let size_bytes: i64 = row
        .try_get("size_bytes")
        .map_err(|e| map_row_err("file_versions", &e))?;
    let produced_by: String = row
        .try_get("produced_by")
        .map_err(|e| map_row_err("file_versions", &e))?;
    let produced_from: Option<i64> = row
        .try_get("produced_from_version_id")
        .map_err(|e| map_row_err("file_versions", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("file_versions", &e))?;
    let retired_at: Option<String> = row
        .try_get("retired_at")
        .map_err(|e| map_row_err("file_versions", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("file_versions", &e))?;
    let size = u64::try_from(size_bytes).map_err(|_| {
        VoomError::Database(format!("file_versions.size_bytes negative ({size_bytes})"))
    })?;
    Ok(FileVersion {
        id: FileVersionId(u64_from_i64(id)),
        file_asset_id: FileAssetId(u64_from_i64(file_asset_id)),
        content_hash,
        size_bytes: size,
        produced_by: ProducedBy::parse(&produced_by)?,
        produced_from_version_id: produced_from.map(|v| FileVersionId(u64_from_i64(v))),
        created_at: parse_iso8601(&created_at)?,
        retired_at: retired_at.map(|s| parse_iso8601(&s)).transpose()?,
        epoch: u64_from_i64(epoch),
    })
}

const SELECT_FILE_LOCATION_COLS: &str = "SELECT id, file_version_id, kind, value, proof_kind, proof_value, \
            observed_at, retired_at, epoch \
     FROM file_locations WHERE id = ?";

async fn get_file_location_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileLocationId,
) -> Result<Option<FileLocation>, VoomError> {
    let row = sqlx::query(SELECT_FILE_LOCATION_COLS)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("file_locations get_in_tx: {e}")))?;
    row.as_ref().map(row_to_file_location).transpose()
}

fn row_to_file_location(row: &sqlx::sqlite::SqliteRow) -> Result<FileLocation, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("file_locations", &e))?;
    let file_version_id: i64 = row
        .try_get("file_version_id")
        .map_err(|e| map_row_err("file_locations", &e))?;
    let kind: String = row
        .try_get("kind")
        .map_err(|e| map_row_err("file_locations", &e))?;
    let value: String = row
        .try_get("value")
        .map_err(|e| map_row_err("file_locations", &e))?;
    let proof_kind: Option<String> = row
        .try_get("proof_kind")
        .map_err(|e| map_row_err("file_locations", &e))?;
    let proof_value: Option<String> = row
        .try_get("proof_value")
        .map_err(|e| map_row_err("file_locations", &e))?;
    let observed_at: String = row
        .try_get("observed_at")
        .map_err(|e| map_row_err("file_locations", &e))?;
    let retired_at: Option<String> = row
        .try_get("retired_at")
        .map_err(|e| map_row_err("file_locations", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("file_locations", &e))?;
    // Validate kind round-trips.
    let _ = FileLocationKind::parse(&kind)?;
    Ok(FileLocation {
        id: FileLocationId(u64_from_i64(id)),
        file_version_id: FileVersionId(u64_from_i64(file_version_id)),
        kind: FileLocationKind::parse(&kind)?,
        value,
        proof_kind,
        proof_value,
        observed_at: parse_iso8601(&observed_at)?,
        retired_at: retired_at.map(|s| parse_iso8601(&s)).transpose()?,
        epoch: u64_from_i64(epoch),
    })
}

const SELECT_IDENTITY_EVIDENCE_COLS: &str = "SELECT id, target_type, target_id, assertion_type, candidate_id, candidate_value, \
            provider, provider_version, confidence, provenance, observed_at, \
            superseded_at, superseded_by_id, accepted_at, accepted_user_id, \
            accepted_policy_id, pinned_file_version_ids, pinned_hashes, pinned_locations \
     FROM identity_evidence WHERE id = ?";

async fn get_identity_evidence_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: EvidenceId,
) -> Result<Option<IdentityEvidence>, VoomError> {
    let row = sqlx::query(SELECT_IDENTITY_EVIDENCE_COLS)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("identity_evidence get_in_tx: {e}")))?;
    row.as_ref().map(row_to_identity_evidence).transpose()
}

fn row_to_identity_evidence(row: &sqlx::sqlite::SqliteRow) -> Result<IdentityEvidence, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let target_type: String = row
        .try_get("target_type")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let target_id: i64 = row
        .try_get("target_id")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let assertion_type: String = row
        .try_get("assertion_type")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let candidate_id: Option<i64> = row
        .try_get("candidate_id")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let candidate_value: Option<String> = row
        .try_get("candidate_value")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let provider: String = row
        .try_get("provider")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let provider_version: String = row
        .try_get("provider_version")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let confidence: f64 = row
        .try_get("confidence")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let provenance: String = row
        .try_get("provenance")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let observed_at: String = row
        .try_get("observed_at")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let superseded_at: Option<String> = row
        .try_get("superseded_at")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let superseded_by_id: Option<i64> = row
        .try_get("superseded_by_id")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let accepted_at: Option<String> = row
        .try_get("accepted_at")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let accepted_user_id: Option<String> = row
        .try_get("accepted_user_id")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let accepted_policy_id: Option<i64> = row
        .try_get("accepted_policy_id")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let pinned_fv: Option<String> = row
        .try_get("pinned_file_version_ids")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let pinned_hashes: Option<String> = row
        .try_get("pinned_hashes")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let pinned_locations: Option<String> = row
        .try_get("pinned_locations")
        .map_err(|e| map_row_err("identity_evidence", &e))?;
    let provenance_v: JsonValue = serde_json::from_str(&provenance)
        .map_err(|e| VoomError::Database(format!("parse provenance: {e}")))?;
    let parse_opt =
        |s: Option<String>, col: &'static str| -> Result<Option<JsonValue>, VoomError> {
            s.map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|e| VoomError::Database(format!("parse {col}: {e}")))
        };
    Ok(IdentityEvidence {
        id: EvidenceId(u64_from_i64(id)),
        target_type: IdentityEvidenceTarget::parse(&target_type)?,
        target_id: u64_from_i64(target_id),
        assertion_type: AssertionKind::from_str(&assertion_type)?,
        candidate_id: candidate_id.map(u64_from_i64),
        candidate_value,
        provider,
        provider_version,
        confidence,
        provenance: provenance_v,
        observed_at: parse_iso8601(&observed_at)?,
        superseded_at: superseded_at.map(|s| parse_iso8601(&s)).transpose()?,
        superseded_by_id: superseded_by_id.map(|v| EvidenceId(u64_from_i64(v))),
        accepted_at: accepted_at.map(|s| parse_iso8601(&s)).transpose()?,
        accepted_user_id,
        accepted_policy_id: accepted_policy_id.map(u64_from_i64),
        pinned_file_version_ids: parse_opt(pinned_fv, "pinned_file_version_ids")?,
        pinned_hashes: parse_opt(pinned_hashes, "pinned_hashes")?,
        pinned_locations: parse_opt(pinned_locations, "pinned_locations")?,
    })
}

const SELECT_MEDIA_SNAPSHOT_COLS: &str = "SELECT id, file_version_id, probed_by, probed_at, payload \
     FROM media_snapshots WHERE id = ?";

async fn get_media_snapshot_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: MediaSnapshotId,
) -> Result<Option<MediaSnapshot>, VoomError> {
    let row = sqlx::query(SELECT_MEDIA_SNAPSHOT_COLS)
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("media_snapshots get_in_tx: {e}")))?;
    row.as_ref().map(row_to_media_snapshot).transpose()
}

fn row_to_media_snapshot(row: &sqlx::sqlite::SqliteRow) -> Result<MediaSnapshot, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("media_snapshots", &e))?;
    let file_version_id: i64 = row
        .try_get("file_version_id")
        .map_err(|e| map_row_err("media_snapshots", &e))?;
    let probed_by: Option<i64> = row
        .try_get("probed_by")
        .map_err(|e| map_row_err("media_snapshots", &e))?;
    let probed_at: String = row
        .try_get("probed_at")
        .map_err(|e| map_row_err("media_snapshots", &e))?;
    let payload: String = row
        .try_get("payload")
        .map_err(|e| map_row_err("media_snapshots", &e))?;
    let payload_v: JsonValue = serde_json::from_str(&payload)
        .map_err(|e| VoomError::Database(format!("parse media_snapshots.payload: {e}")))?;
    Ok(MediaSnapshot {
        id: MediaSnapshotId(u64_from_i64(id)),
        file_version_id: FileVersionId(u64_from_i64(file_version_id)),
        probed_by: probed_by.map(|v| WorkerId(u64_from_i64(v))),
        probed_at: parse_iso8601(&probed_at)?,
        payload: payload_v,
    })
}

#[cfg(test)]
#[path = "identity_test.rs"]
mod tests;
