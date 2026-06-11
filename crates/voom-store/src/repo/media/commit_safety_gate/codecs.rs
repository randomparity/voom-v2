use super::{
    AffectedScopeClosure, BTreeSet, BundleId, ClosureWarning, CommitTarget, Deserialize,
    EvidenceId, FileAssetId, FileLocationId, FileLocationKind, FileLocationProposal, FileVersionId,
    ForcePathToken, JsonValue, LocationProof, OffsetDateTime, Serialize, TargetMemberKind,
    VoomError,
};

// ----- JSON wire formats for the `commit_intents` JSON columns ----------------
//
// `commit_intents.target` and `commit_intents.closure_initial` are
// JSON-encoded; `accepted_evidence_ids` is a JSON array. The Rust-side
// public types intentionally do NOT derive `Serialize`/`Deserialize`
// (some embed M2 types like `FileLocationKind` and `LocationProof`
// that do not derive serde, and adding derives there would force a
// wider M2 touch). Dedicated wire-format structs keep the on-disk JSON
// shape stable and isolated; later commits read the same columns back
// via the inverse mappers.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CommitTargetWire {
    #[serde(rename = "delete_file_location")]
    Delete(DeleteFileLocationWire),
    #[serde(rename = "replace_file_location")]
    Replace(ReplaceFileLocationWire),
    #[serde(rename = "move_file_location")]
    Move(MoveFileLocationWire),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteFileLocationWire {
    retired: FileLocationId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceFileLocationWire {
    retired: FileLocationId,
    new: FileLocationProposalWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveFileLocationWire {
    retired: FileLocationId,
    new: FileLocationProposalWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLocationProposalWire {
    kind: String,
    value: String,
    proof_kind: Option<String>,
    proof_value: Option<String>,
    #[serde(with = "time::serde::iso8601")]
    observed_at: OffsetDateTime,
}

impl FileLocationProposalWire {
    fn from_proposal(p: &FileLocationProposal) -> Self {
        let (proof_kind, proof_value) = match &p.proof {
            None => (None, None),
            Some(proof) => (
                Some(proof_kind_str(proof).to_owned()),
                Some(proof_value_str(proof)),
            ),
        };
        Self {
            kind: p.kind.as_str().to_owned(),
            value: p.value.clone(),
            proof_kind,
            proof_value,
            observed_at: p.observed_at,
        }
    }
}

fn proof_kind_str(proof: &LocationProof) -> &'static str {
    match proof {
        LocationProof::LocalFileIdGeneration { .. } => "file_id_generation",
        LocationProof::ObjectStoreVersion { .. } => "object_version_id",
    }
}

fn proof_value_str(proof: &LocationProof) -> String {
    match proof {
        LocationProof::LocalFileIdGeneration {
            file_id,
            generation,
        } => serde_json::json!({
            "file_id": file_id.to_string(),
            "generation": generation,
        })
        .to_string(),
        LocationProof::ObjectStoreVersion {
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

fn commit_target_to_wire(t: &CommitTarget) -> CommitTargetWire {
    match t {
        CommitTarget::DeleteFileLocation(id) => {
            CommitTargetWire::Delete(DeleteFileLocationWire { retired: *id })
        }
        CommitTarget::ReplaceFileLocation { retired, new } => {
            CommitTargetWire::Replace(ReplaceFileLocationWire {
                retired: *retired,
                new: FileLocationProposalWire::from_proposal(new),
            })
        }
        CommitTarget::MoveFileLocation { retired, new } => {
            CommitTargetWire::Move(MoveFileLocationWire {
                retired: *retired,
                new: FileLocationProposalWire::from_proposal(new),
            })
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AffectedScopeClosureWire {
    file_assets: BTreeSet<FileAssetId>,
    file_versions: BTreeSet<FileVersionId>,
    file_locations: BTreeSet<FileLocationId>,
    bundles: BTreeSet<BundleId>,
    resolution_warnings: Vec<ClosureWarningWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosureWarningWire {
    message: String,
}

fn closure_to_wire(c: &AffectedScopeClosure) -> AffectedScopeClosureWire {
    AffectedScopeClosureWire {
        file_assets: c.file_assets.clone(),
        file_versions: c.file_versions.clone(),
        file_locations: c.file_locations.clone(),
        bundles: c.bundles.clone(),
        resolution_warnings: c
            .resolution_warnings
            .iter()
            .map(|w| ClosureWarningWire {
                message: w.message.clone(),
            })
            .collect(),
    }
}

pub(super) fn encode_target(t: &CommitTarget) -> Result<String, VoomError> {
    serde_json::to_string(&commit_target_to_wire(t))
        .map_err(|e| VoomError::Internal(format!("encode commit_target: {e}")))
}

pub(super) fn encode_closure(c: &AffectedScopeClosure) -> Result<String, VoomError> {
    serde_json::to_string(&closure_to_wire(c))
        .map_err(|e| VoomError::Internal(format!("encode closure: {e}")))
}

pub(super) fn encode_evidence_ids(ids: &[EvidenceId]) -> Result<String, VoomError> {
    serde_json::to_string(ids).map_err(|e| VoomError::Internal(format!("encode evidence_ids: {e}")))
}

/// JSON-encode a `ForcePathToken` for the
/// `commit_intents.override_token` column. Uses the struct's derived
/// serde — `actor` / `reason` are plain strings; `bypass: BTreeSet<BypassKind>`
/// serializes as an ordered JSON array of `snake_case` tags
/// such as `["closure_incomplete"]`. The decoder (`decode_force_path_token`)
/// is the inverse.
pub(super) fn encode_force_path_token(token: &ForcePathToken) -> Result<String, VoomError> {
    serde_json::to_string(token)
        .map_err(|e| VoomError::Internal(format!("encode override_token: {e}")))
}

/// Inverse of `encode_force_path_token`. The
/// `commit_intents.override_token` column is written exclusively by
/// `prepare_destructive_commit` and never mutated; a parse failure is
/// `VoomError::Database` because that's the on-disk corruption case.
pub(super) fn decode_force_path_token(json: &str) -> Result<ForcePathToken, VoomError> {
    serde_json::from_str(json)
        .map_err(|e| VoomError::Database(format!("decode override_token: {e}")))
}

// ----- inverse wire-format decoders -----------------------------------------
//
// Phase B reads back the JSON columns that Phase A wrote (`target`,
// `closure_initial`) so it can re-emit closure-grew payloads and surface
// state through `PendingCommitIntent`. The decoders mirror the encoder
// shapes exactly — they are deliberately module-private so the on-disk
// JSON contract has a single owning module.

pub(super) fn decode_target(json: &str) -> Result<CommitTarget, VoomError> {
    let wire: CommitTargetWire = serde_json::from_str(json)
        .map_err(|e| VoomError::Database(format!("decode commit_target: {e}")))?;
    commit_target_from_wire(wire)
}

pub(super) fn decode_closure(json: &str) -> Result<AffectedScopeClosure, VoomError> {
    let wire: AffectedScopeClosureWire = serde_json::from_str(json)
        .map_err(|e| VoomError::Database(format!("decode closure: {e}")))?;
    Ok(closure_from_wire(wire))
}

fn commit_target_from_wire(w: CommitTargetWire) -> Result<CommitTarget, VoomError> {
    Ok(match w {
        CommitTargetWire::Delete(DeleteFileLocationWire { retired }) => {
            CommitTarget::DeleteFileLocation(retired)
        }
        CommitTargetWire::Replace(ReplaceFileLocationWire { retired, new }) => {
            CommitTarget::ReplaceFileLocation {
                retired,
                new: file_location_proposal_from_wire(new)?,
            }
        }
        CommitTargetWire::Move(MoveFileLocationWire { retired, new }) => {
            CommitTarget::MoveFileLocation {
                retired,
                new: file_location_proposal_from_wire(new)?,
            }
        }
    })
}

fn file_location_proposal_from_wire(
    w: FileLocationProposalWire,
) -> Result<FileLocationProposal, VoomError> {
    let proof = decode_proof(w.proof_kind.as_deref(), w.proof_value.as_deref())?;
    Ok(FileLocationProposal {
        kind: FileLocationKind::parse(&w.kind)?,
        value: w.value,
        proof,
        observed_at: w.observed_at,
    })
}

fn decode_proof(
    kind: Option<&str>,
    value: Option<&str>,
) -> Result<Option<LocationProof>, VoomError> {
    let (Some(kind), Some(value)) = (kind, value) else {
        return Ok(None);
    };
    let parsed: JsonValue = serde_json::from_str(value)
        .map_err(|e| VoomError::Database(format!("decode proof_value: {e}")))?;
    match kind {
        "file_id_generation" => {
            let file_id = parsed
                .get("file_id")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| VoomError::Database("decode proof: missing file_id".to_owned()))?
                .parse::<u128>()
                .map_err(|e| VoomError::Database(format!("decode proof: file_id u128: {e}")))?;
            let generation = parsed
                .get("generation")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    VoomError::Database("decode proof: missing generation".to_owned())
                })?;
            Ok(Some(LocationProof::LocalFileIdGeneration {
                file_id,
                generation,
            }))
        }
        "object_version_id" => {
            let bucket = parsed
                .get("bucket")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| VoomError::Database("decode proof: missing bucket".to_owned()))?
                .to_owned();
            let key = parsed
                .get("key")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| VoomError::Database("decode proof: missing key".to_owned()))?
                .to_owned();
            let version_id = parsed
                .get("version_id")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| VoomError::Database("decode proof: missing version_id".to_owned()))?
                .to_owned();
            Ok(Some(LocationProof::ObjectStoreVersion {
                bucket,
                key,
                version_id,
            }))
        }
        other => Err(VoomError::Database(format!(
            "decode proof: unknown kind {other:?}"
        ))),
    }
}

fn closure_from_wire(w: AffectedScopeClosureWire) -> AffectedScopeClosure {
    AffectedScopeClosure {
        file_assets: w.file_assets,
        file_versions: w.file_versions,
        file_locations: w.file_locations,
        bundles: w.bundles,
        resolution_warnings: w
            .resolution_warnings
            .into_iter()
            .map(|w| ClosureWarning { message: w.message })
            .collect(),
    }
}

// ----- per-member epoch snapshot wire format --------------------------------
//
// `commit_intents.target_row_epochs` is a JSON array of [kind, row_id,
// epoch] triples. Phase B writes it atomically with `state='authorized'`;
// Phase C re-reads it and uses each `epoch` as the `expected_epoch`
// argument to the matching `IdentityRepo` destructive mutation. `kind`
// round-trips through `TargetMemberKind`'s `Serialize/Deserialize` impl
// (`#[serde(rename_all = "snake_case")]`).

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TargetRowEpochTriple(pub(super) TargetMemberKind, pub(super) u64, pub(super) u64);

pub(super) fn encode_target_row_epochs(
    triples: &[TargetRowEpochTriple],
) -> Result<String, VoomError> {
    serde_json::to_string(triples)
        .map_err(|e| VoomError::Internal(format!("encode target_row_epochs: {e}")))
}

#[cfg(test)]
#[path = "codecs_test.rs"]
mod tests;
