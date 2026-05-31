use super::prepare::{GatePhase, PhaseAAbort};
use super::{
    AffectedScopeClosure, AliasResolutionError, AliasResolver, BTreeSet, BundleId, BypassKind,
    ClosureWarning, CommitId, CommitTarget, EvidenceDrift, EvidenceId, EvidenceRevalidationResult,
    FileAssetId, FileLocationId, FileVersionId, IdentityEvidenceTarget, IdentityRepo, JsonValue,
    LeaseScope, Row, UseLeaseId, VoomError, i64_from_u64, u64_from_i64,
};

/// Resolve the destructive target into the set of `FileLocation` rows
/// the closure walk anchors on. Returns `None` if the target's retired
/// row is missing or already terminal — Phase C will trip the epoch
/// guard regardless, but Phase A surfaces it eagerly as a closure-walk
/// failure so the operator does not wait for the round trip.
///
/// `bypass` is the active force-path bypass set (commit 10). When it
/// contains `BypassKind::ClosureIncomplete`, an
/// `AliasResolutionError::Unreachable` from the external resolver is
/// swallowed: the walk proceeds with whatever DB-internal aliases were
/// already enumerated rather than aborting. Phase C does not pipe this
/// flag through (the bypass is consumed once at prepare and re-applied
/// at authorize; Phase C's `Authorize` walker never receives it because
/// the closure walker only surfaces `Unreachable` when a fresh
/// `AliasResolutionError::Unreachable` fires — see `run_phase_c_trip_wires_in_tx`'s
/// internal-error escape on closure-incomplete at finalize).
pub(super) async fn build_closure(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    alias_resolver: &dyn AliasResolver,
    target: &CommitTarget,
    phase: GatePhase,
    bypass: &BTreeSet<BypassKind>,
) -> Result<Result<(AffectedScopeClosure, Vec<FileLocationId>), PhaseAAbort>, VoomError> {
    let retired_location_id = match target {
        CommitTarget::DeleteFileLocation(id) => *id,
        CommitTarget::ReplaceFileLocation { retired, .. }
        | CommitTarget::MoveFileLocation { retired, .. } => *retired,
    };

    let location = identity_repo
        .get_file_location_in_tx(tx, retired_location_id)
        .await?;
    let Some(location) = location else {
        return Ok(Err(PhaseAAbort::ClosureIncomplete {
            message: format!("target file_location {retired_location_id} not found"),
        }));
    };
    // Phase A surfaces an already-retired target as
    // closure-incomplete (operator handed a stale handle; abort
    // eagerly so the audit row records the precondition trip). Phase B
    // is structurally different: a target that became retired between
    // prepare and authorize is closure drift, not closure-incomplete —
    // the recomputed closure simply loses the retired row and (often)
    // gains the rename-introduced replacement, surfacing as
    // `BlockedByClosureGrew` further down. The Phase-A trip-wire would
    // mask the drift signal e2e callers depend on, so it stays
    // Phase-A-gated.
    if phase == GatePhase::Prepare && location.retired_at.is_some() {
        return Ok(Err(PhaseAAbort::ClosureIncomplete {
            message: format!("target file_location {retired_location_id} already retired"),
        }));
    }

    let version = identity_repo
        .get_file_version_in_tx(tx, location.file_version_id)
        .await?;
    let Some(version) = version else {
        return Ok(Err(PhaseAAbort::ClosureIncomplete {
            message: format!("target file_version {} not found", location.file_version_id),
        }));
    };

    // DB-internal live alias enumeration on the same tx (round-5 fix).
    let live_locations: BTreeSet<FileLocationId> = identity_repo
        .list_live_file_locations_by_version_in_tx(tx, version.id)
        .await?
        .into_iter()
        .collect();

    // External alias enumeration through the trait — Sprint 1 ships
    // only `FailingAliasResolver`, which returns `Unreachable` to drive
    // the closure-incomplete abort branch in tests.
    let mut alias_warnings: Vec<ClosureWarning> = Vec::new();
    let mut external_locations: BTreeSet<FileLocationId> = BTreeSet::new();
    match alias_resolver.aliases_for_version(version.id).await {
        Ok(extra) => {
            for id in extra {
                external_locations.insert(id);
            }
        }
        Err(AliasResolutionError::Unreachable { message }) => {
            // Force-path bypass: a token carrying
            // `BypassKind::ClosureIncomplete` suppresses the abort.
            // The walk continues with the partial closure (the
            // external resolver's contribution is lost; the
            // DB-internal `live_locations` already in hand are the
            // best evidence the gate has). The bypass is recorded
            // separately via `commit.forced_override` — the absence
            // of `commit.aborted_by_closure_incomplete` is the
            // visible difference in the audit trail.
            if bypass.contains(&BypassKind::ClosureIncomplete) {
                alias_warnings.push(ClosureWarning {
                    message: format!("force-path bypass honored: {message}"),
                });
            } else {
                return Ok(Err(PhaseAAbort::ClosureIncomplete { message }));
            }
        }
        Err(AliasResolutionError::Database(msg)) => {
            return Err(VoomError::Database(format!("alias resolver: {msg}")));
        }
    }

    let mut file_locations = live_locations;
    for id in external_locations {
        file_locations.insert(id);
    }
    // Phase A guards against the target already being terminal upstream,
    // so a non-terminal target is always live and already present in
    // `live_locations`; the defense-in-depth insert here keeps the
    // invariant explicit (no-op if the row is live; pins the target
    // member when the live-listing query and the target's row state
    // diverge mid-walk). Phase B is structurally different: a retired
    // target should fall OUT of the closure (closure drift signal), so
    // re-adding it here would mask the trip-wire.
    if phase == GatePhase::Prepare {
        file_locations.insert(retired_location_id);
    }

    // Bundle membership for the owning FileAsset.
    let mut bundles: BTreeSet<BundleId> = BTreeSet::new();
    let bundle_rows: Vec<i64> =
        sqlx::query_scalar("SELECT bundle_id FROM asset_bundle_members WHERE file_asset_id = ?")
            .bind(i64_from_u64(version.file_asset_id.0))
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("asset_bundle_members lookup: {e}")))?;
    for raw in bundle_rows {
        bundles.insert(BundleId(u64_from_i64(raw)));
    }

    let mut file_versions: BTreeSet<FileVersionId> = BTreeSet::new();
    file_versions.insert(version.id);

    let mut file_assets: BTreeSet<FileAssetId> = BTreeSet::new();
    file_assets.insert(version.file_asset_id);

    // Warnings stay empty unless the force-path bypass swallowed an
    // `Unreachable` (commit 10) — in which case the dropped resolver
    // message rides along on the closure as a non-fatal annotation.
    // Round-3 invariant: warnings do NOT contribute to closure drift
    // (`id_member_delta` ignores them), so the bypass-introduced
    // warning cannot mask the Phase B closure-grew trip-wire.
    let closure = AffectedScopeClosure {
        file_assets,
        file_versions,
        file_locations: file_locations.clone(),
        bundles,
        resolution_warnings: std::mem::take(&mut alias_warnings),
    };
    let target_locations: Vec<FileLocationId> = file_locations.into_iter().collect();
    Ok(Ok((closure, target_locations)))
}

/// Read every live blocking use-lease whose scope_*_id column matches a
/// member of `closure`. Returns the list of `UseLeaseId`s evaluated
/// (used as `CommitIntent.evaluated_lease_ids` on the success path).
pub(super) async fn list_blocking_leases_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Vec<UseLeaseId>, VoomError> {
    let mut ids: Vec<UseLeaseId> = Vec::new();
    let raw_rows = blocking_lease_rows_in_tx(tx, closure).await?;
    for (id, _) in raw_rows {
        ids.push(id);
    }
    Ok(ids)
}

/// First overlap between a live blocking use-lease and `closure`. The
/// return shape carries both the lease id and the lease's scope so the
/// abort payload can report the offending scope without a second
/// lookup.
pub(super) async fn first_blocking_overlap_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Option<(UseLeaseId, LeaseScope)>, VoomError> {
    Ok(blocking_lease_rows_in_tx(tx, closure)
        .await?
        .into_iter()
        .next())
}

/// Underlying query: returns every (`lease_id`, scope) pair where the
/// lease is live (`release_reason IS NULL`), blocking, and its scope
/// matches a member of `closure`. Ordered by `id ASC` so the
/// "first overlap" path is deterministic across test runs.
async fn blocking_lease_rows_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Vec<(UseLeaseId, LeaseScope)>, VoomError> {
    if closure.file_assets.is_empty()
        && closure.bundles.is_empty()
        && closure.file_versions.is_empty()
        && closure.file_locations.is_empty()
    {
        return Ok(Vec::new());
    }
    let assets_json = serde_json::to_string(&closure.file_assets)
        .map_err(|e| VoomError::Internal(format!("encode closure.file_assets: {e}")))?;
    let bundles_json = serde_json::to_string(&closure.bundles)
        .map_err(|e| VoomError::Internal(format!("encode closure.bundles: {e}")))?;
    let versions_json = serde_json::to_string(&closure.file_versions)
        .map_err(|e| VoomError::Internal(format!("encode closure.file_versions: {e}")))?;
    let locations_json = serde_json::to_string(&closure.file_locations)
        .map_err(|e| VoomError::Internal(format!("encode closure.file_locations: {e}")))?;

    // SQLite `json_each` produces one row per element of the bound JSON
    // array; the UNION ALL across the four scope columns is the
    // four-granularity overlap check from §9.3. `release_reason IS NULL`
    // restricts to live leases; `blocking_mode = 'blocking'` honors the
    // arch-spec distinction between blocking and advisory.
    let rows = sqlx::query(
        "SELECT id, scope_asset_id, scope_bundle_id, scope_version_id, scope_location_id \
         FROM asset_use_leases \
         WHERE release_reason IS NULL AND blocking_mode = 'blocking' AND ( \
             scope_asset_id    IN (SELECT value FROM json_each(?)) \
          OR scope_bundle_id   IN (SELECT value FROM json_each(?)) \
          OR scope_version_id  IN (SELECT value FROM json_each(?)) \
          OR scope_location_id IN (SELECT value FROM json_each(?)) \
         ) \
         ORDER BY id ASC",
    )
    .bind(&assets_json)
    .bind(&bundles_json)
    .bind(&versions_json)
    .bind(&locations_json)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("blocking-lease overlap: {e}")))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row
            .try_get("id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let sa: Option<i64> = row
            .try_get("scope_asset_id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let sb: Option<i64> = row
            .try_get("scope_bundle_id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let sv: Option<i64> = row
            .try_get("scope_version_id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let sl: Option<i64> = row
            .try_get("scope_location_id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let scope = match (sa, sb, sv, sl) {
            (Some(v), None, None, None) => LeaseScope::Asset(FileAssetId(u64_from_i64(v))),
            (None, Some(v), None, None) => LeaseScope::Bundle(BundleId(u64_from_i64(v))),
            (None, None, Some(v), None) => LeaseScope::Version(FileVersionId(u64_from_i64(v))),
            (None, None, None, Some(v)) => LeaseScope::Location(FileLocationId(u64_from_i64(v))),
            other => {
                return Err(VoomError::Database(format!(
                    "blocking-lease row: scope_*_id columns are not exactly-one: {other:?}"
                )));
            }
        };
        out.push((UseLeaseId(u64_from_i64(id)), scope));
    }
    Ok(out)
}

/// Revalidate every accepted-evidence pin against current state. For
/// each `evidence_id`, look up the row inside the gate's tx, decode the
/// pinned columns, and compare against current state. Returns one
/// result per id (`drift = None` for pins that still match).
pub(super) async fn revalidate_evidence_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    ids: &[EvidenceId],
) -> Result<Vec<EvidenceRevalidationResult>, VoomError> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let evidence = identity_repo
            .get_identity_evidence_in_tx(tx, *id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("identity_evidence {id} not found")))?;
        // Phase A only consults accepted rows. Treat un-accepted /
        // superseded pins as drift so the gate cannot proceed against
        // evidence that no longer carries an authoritative pin.
        if evidence.accepted_at.is_none() {
            out.push(EvidenceRevalidationResult {
                evidence_id: *id,
                drift: Some(EvidenceDrift::PinnedFileVersionRetired),
            });
            continue;
        }

        let drift = first_evidence_pin_drift(tx, &evidence).await?;
        out.push(EvidenceRevalidationResult {
            evidence_id: *id,
            drift,
        });
        // `IdentityEvidenceTarget` exists in `identity.rs` and is
        // imported here so the round-trip parsing of the row's
        // `target_type` is the single source of truth; the variant
        // itself is unused in Sprint 1 evidence revalidation.
        let _ = std::marker::PhantomData::<IdentityEvidenceTarget>;
    }
    Ok(out)
}

async fn first_evidence_pin_drift(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    evidence: &crate::repo::media::identity::IdentityEvidence,
) -> Result<Option<EvidenceDrift>, VoomError> {
    // Pinned FileVersion IDs — any retired version → drift.
    if let Some(versions_json) = &evidence.pinned_file_version_ids {
        for vid in pinned_u64_array(versions_json, "pinned_file_version_ids")? {
            if version_is_retired(tx, FileVersionId(vid)).await? {
                return Ok(Some(EvidenceDrift::PinnedFileVersionRetired));
            }
        }
    }
    // Pinned locations — any retired location → drift.
    if let Some(locs_json) = &evidence.pinned_locations {
        for lid in pinned_u64_array(locs_json, "pinned_locations")? {
            if location_is_retired(tx, FileLocationId(lid)).await? {
                return Ok(Some(EvidenceDrift::PinnedLocationRetired));
            }
        }
    }
    // Pinned hashes — compare against current `file_versions.content_hash`.
    // The pin shape ships as `[ [version_id, hash], ... ]` per sprint
    // §8.7; rows where the stored hash no longer matches drive the
    // `PinnedHashDiffers` exit.
    if let Some(hashes_json) = &evidence.pinned_hashes {
        for (vid, expected) in pinned_hash_pairs(hashes_json, "pinned_hashes")? {
            if let Some(current) = version_content_hash(tx, FileVersionId(vid)).await? {
                if current != expected {
                    return Ok(Some(EvidenceDrift::PinnedHashDiffers));
                }
            } else {
                // Pinned to a version that no longer exists — surface
                // the retired-version drift kind so the operator's
                // diagnostic path is consistent.
                return Ok(Some(EvidenceDrift::PinnedFileVersionRetired));
            }
        }
    }
    Ok(None)
}

fn pinned_u64_array(value: &JsonValue, field: &str) -> Result<Vec<u64>, VoomError> {
    let arr = value
        .as_array()
        .ok_or_else(|| VoomError::Database(format!("{field}: expected JSON array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let n = v
            .as_u64()
            .ok_or_else(|| VoomError::Database(format!("{field}: expected u64 element")))?;
        out.push(n);
    }
    Ok(out)
}

fn pinned_hash_pairs(value: &JsonValue, field: &str) -> Result<Vec<(u64, String)>, VoomError> {
    let arr = value
        .as_array()
        .ok_or_else(|| VoomError::Database(format!("{field}: expected JSON array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for pair in arr {
        let row = pair
            .as_array()
            .ok_or_else(|| VoomError::Database(format!("{field}: expected JSON array element")))?;
        if row.len() != 2 {
            return Err(VoomError::Database(format!(
                "{field}: expected 2-element [version_id, hash] arrays"
            )));
        }
        let vid = row[0]
            .as_u64()
            .ok_or_else(|| VoomError::Database(format!("{field}: version_id not u64")))?;
        let hash = row[1]
            .as_str()
            .ok_or_else(|| VoomError::Database(format!("{field}: hash not str")))?
            .to_owned();
        out.push((vid, hash));
    }
    Ok(out)
}

async fn version_is_retired(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileVersionId,
) -> Result<bool, VoomError> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT retired_at FROM file_versions WHERE id = ?")
            .bind(i64_from_u64(id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("file_versions retired probe: {e}")))?;
    Ok(matches!(row, Some(Some(_))))
}

async fn location_is_retired(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileLocationId,
) -> Result<bool, VoomError> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(i64_from_u64(id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("file_locations retired probe: {e}")))?;
    Ok(matches!(row, Some(Some(_))))
}

async fn version_content_hash(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileVersionId,
) -> Result<Option<String>, VoomError> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT content_hash FROM file_versions WHERE id = ?")
            .bind(i64_from_u64(id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("file_versions hash probe: {e}")))?;
    Ok(row)
}

pub(super) fn first_evidence_drift(
    results: &[EvidenceRevalidationResult],
) -> Option<(EvidenceId, &EvidenceDrift)> {
    for r in results {
        if let Some(d) = &r.drift {
            return Some((r.evidence_id, d));
        }
    }
    None
}

/// Insert one `commit_intent_scope_members` row per closure member,
/// across all four granularities. Per migration 0005's CHECK exactly
/// one of the four `scope_*_id` columns is non-NULL per row.
pub(super) async fn expand_scope_members(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    closure: &AffectedScopeClosure,
) -> Result<(), VoomError> {
    let cid = i64_from_u64(commit_id.0);
    for id in &closure.file_assets {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_asset_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members asset insert: {e}")))?;
    }
    for id in &closure.bundles {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_bundle_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members bundle insert: {e}")))?;
    }
    for id in &closure.file_versions {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_version_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members version insert: {e}")))?;
    }
    for id in &closure.file_locations {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_location_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members location insert: {e}")))?;
    }
    Ok(())
}
