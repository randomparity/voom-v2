//! Fenced node-local commit-intent executor (ADR 0074 plan Task 6).
//!
//! Polls the open-intent listing and drives each intent through the HTTP
//! commit routes exactly as the reference node behavior
//! (`voom-test-support::commit_node::drive_pending_commit`): authorize,
//! journal `applying` BEFORE touching any byte, promote the staged bytes
//! no-replace (an already-promoted matching target counts as applied; a
//! differing target is mismatched), report typed outcome evidence, and
//! complete with the one-time fence.
//!
//! Crash-window semantics: the `applying` receipt is journaled before any
//! byte mutation, so an interrupted promotion is always discoverable. A
//! target that already holds matching bytes converges via `applied` evidence
//! without a rewrite. An intent observed in the `authorized` state whose
//! authorize request this incarnation never issued is left for the
//! control-plane recovery classification (spec step 7) — a fresh authorize
//! against a non-pending intent is refused by design.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rand::rngs::StdRng;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use voom_core::ids::ArtifactCommitIntentId;
use voom_core::{NodeId, NodeIncarnationId, StorageRootId, VoomError};

use crate::client::{
    CommitAppliedEvidence, CommitApplyingRequest, CommitAuthorizeOutcome, CommitAuthorizeRequest,
    CommitCompleteOutcome, CommitCompleteRequest, CommitMismatchedEvidence, CommitObservedFacts,
    CommitOpenOutcome, CommitOpenRequest, CommitOutcomeEvidence, CommitOutcomeRequest,
    CommitOutcomeUnknownEvidence, CommitReceiptOutcome, OpenCommitIntent, RetryRequest,
};
use crate::runtime::{
    ControlPlaneApi, CoordinatorExit, LeaseSettlement, RuntimeFatal, ShutdownForce, ShutdownKind,
    centered_jitter, new_key, until_shutdown,
};

/// Prefix of the temp-sibling naming ported from the retired host-side
/// promoter (`unique_temp_sibling_path`); recovery classifies an interrupted
/// promotion by the presence of these siblings next to the target.
const TEMP_SIBLING_PREFIX: &str = ".voom-tmp.";
/// Reason recorded when the target is absent with no temp sibling: positive
/// evidence that promotion never happened. Must match the control plane's
/// `RESOLVED_NOT_APPLIED_REASON`.
const RESOLVED_NOT_APPLIED_REASON: &str = "target_absent_no_temp_sibling";
const READ_BUFFER_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct CommitCoordinatorContext {
    pub(crate) api: Arc<dyn ControlPlaneApi>,
    pub(crate) node_id: NodeId,
    pub(crate) incarnation_id: NodeIncarnationId,
    pub(crate) poll_interval: Duration,
    /// Filesystem provider locator per storage-root id, from agent config.
    pub(crate) storage_roots: HashMap<u64, PathBuf>,
}

#[derive(Debug)]
enum Promotion {
    Applied(CommitObservedFacts),
    Mismatched {
        reason: String,
        observed: Option<CommitObservedFacts>,
    },
}

enum InstallError {
    /// The target appeared between the absence check and the no-replace link:
    /// a concurrent or prior crashed attempt owns the name.
    TargetAppeared,
    Failed(VoomError),
}

/// Run the commit-intent executor until shutdown.
///
/// Each cycle drives every open intent to convergence (or records typed
/// drift evidence), then sleeps a jittered poll interval. One conflict is
/// classified instead of fatal: an authorize refused as `not pending` (an
/// `authorized` intent minted by a prior incarnation) is skipped and left to
/// control-plane recovery. Any other case error — transport exhaustion after
/// client retries, protocol conflict, fence mismatch, filesystem failure —
/// stands the executor down fatally rather than risking un-journaled
/// mutation.
pub(crate) async fn run_commit_coordinator(
    context: CommitCoordinatorContext,
    mut shutdown: watch::Receiver<ShutdownKind>,
    mut schedule_rng: StdRng,
) -> CoordinatorExit {
    // Frozen authorize requests keyed by intent id. Reusing the frozen
    // idempotency key makes a re-drive of an already-authorized intent hit
    // the control plane's replay path, which returns the same fence.
    let mut authorize_requests: HashMap<u64, RetryRequest<CommitAuthorizeRequest>> = HashMap::new();
    loop {
        // Raced against the shutdown, not awaited bare. This task shares the `JoinSet`
        // that `wait_for_coordinators` joins, and `drive_open_intents` reaches the
        // retrying client — so an unraced call here holds the whole shutdown tail open
        // for `production_request_budget()`, past every budget above it, and the tail
        // ends at its backstop without ever attempting the deactivation. See ADR 0088.
        let Some(driven) = until_shutdown(
            &shutdown,
            drive_open_intents(&context, &mut authorize_requests),
        )
        .await
        else {
            return CoordinatorExit::Shutdown(shutdown_settlement(*shutdown.borrow()));
        };
        if let Err(error) = driven {
            return CoordinatorExit::Fatal(RuntimeFatal::ControlPlane(error));
        }
        let delay = centered_jitter(context.poll_interval, &mut schedule_rng);
        tokio::select! {
            changed = shutdown.changed() => {
                let kind = if changed.is_err() {
                    ShutdownKind::Fenced
                } else {
                    *shutdown.borrow()
                };
                return CoordinatorExit::Shutdown(shutdown_settlement(kind));
            }
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// How this coordinator reports a shutdown it observed.
///
/// A published `Forced` kind only ever comes from `wait_for_coordinators`' signal arm;
/// this coordinator has no budget of its own to expire.
fn shutdown_settlement(kind: ShutdownKind) -> LeaseSettlement {
    if kind == ShutdownKind::Forced {
        LeaseSettlement::Forced(ShutdownForce::Signal)
    } else {
        LeaseSettlement::Completed
    }
}

async fn drive_open_intents(
    context: &CommitCoordinatorContext,
    authorize_requests: &mut HashMap<u64, RetryRequest<CommitAuthorizeRequest>>,
) -> Result<(), VoomError> {
    let listing = open_commit_intents(context).await?;
    for intent in listing.intents {
        match intent.state.as_str() {
            "pending" | "authorized" => {
                drive_commit_intent(context, intent.id, authorize_requests).await?;
            }
            "recovery_required" => drive_recovery_intent(context, &intent).await?,
            other => {
                return Err(VoomError::Internal(format!(
                    "commit intent {} reports unknown open state {other:?}",
                    intent.id
                )));
            }
        }
    }
    Ok(())
}

/// Drive one `pending`/`authorized` intent: authorize (replaying the frozen
/// request when this incarnation already issued it), journal `applying`,
/// promote the staged bytes no-replace, report typed evidence, and complete
/// with the fence on `applied`. Drift reports `mismatched` and stops before
/// completion; nothing but the journaled receipt precedes byte mutation.
async fn drive_commit_intent(
    context: &CommitCoordinatorContext,
    intent_id: ArtifactCommitIntentId,
    authorize_requests: &mut HashMap<u64, RetryRequest<CommitAuthorizeRequest>>,
) -> Result<(), VoomError> {
    let Some(authorized) = authorize_intent(context, intent_id, authorize_requests).await? else {
        // The intent left the pending state between the listing and the
        // authorize — the signature of an `authorized` intent minted by a
        // prior agent incarnation. Per the module contract it belongs to the
        // control plane's recovery classification: skip it and keep the
        // normal poll cadence instead of standing down fatally.
        return Ok(());
    };

    let applying_request = RetryRequest::new(
        new_key("commit-applying"),
        &CommitApplyingRequest {
            node_id: context.node_id,
            incarnation_id: context.incarnation_id,
        },
    )?;
    context
        .api
        .report_commit_applying(intent_id, &applying_request)
        .await?;

    let expected = CommitObservedFacts {
        size_bytes: authorized.expected_size_bytes,
        content_hash: authorized.expected_content_hash.clone(),
    };
    let staging_path = resolve_rooted_path(
        context,
        authorized.staging_storage_root_id,
        &authorized.staging_provider_relative_locator,
    )
    .await?;
    let source_path = resolve_rooted_path(
        context,
        authorized.source_storage_root_id,
        &authorized.source_provider_relative_locator,
    )
    .await?;
    let target_path = resolve_rooted_path(
        context,
        authorized.target_storage_root_id,
        &authorized.target_provider_relative_locator,
    )
    .await?;

    match materialize_staging_from_source(&staging_path, &source_path, &expected).await? {
        Materialize::SourceMismatch { reason, observed } => {
            // The pinned source cannot produce the pinned staged bytes: file
            // the typed drift evidence instead of promoting anything.
            report_outcome(
                context,
                intent_id,
                CommitOutcomeEvidence::Mismatched(CommitMismatchedEvidence {
                    reason,
                    observed: Some(observed),
                }),
            )
            .await?;
            return Ok(());
        }
        Materialize::Present | Materialize::Materialized => {}
    }
    let evidence = match promote_staged_bytes(&staging_path, &target_path, &expected).await? {
        Promotion::Applied(observed) => {
            CommitOutcomeEvidence::Applied(CommitAppliedEvidence { observed })
        }
        Promotion::Mismatched { reason, observed } => {
            CommitOutcomeEvidence::Mismatched(CommitMismatchedEvidence { reason, observed })
        }
    };
    report_outcome(context, intent_id, evidence.clone()).await?;
    if matches!(evidence, CommitOutcomeEvidence::Applied(_)) {
        complete_commit(context, intent_id, &authorized.fence_hex).await?;
    }
    Ok(())
}

/// Authorize one intent, replaying the frozen request when this incarnation
/// already issued it. Returns `Ok(None)` when a fresh authorization is
/// refused because the intent is no longer pending — the control plane's
/// marker that the intent was authorized by an earlier agent incarnation and
/// now belongs to its recovery classification. Any other error, including
/// every other conflict (fence mismatch, recovery disputes), stays fatal.
async fn authorize_intent(
    context: &CommitCoordinatorContext,
    intent_id: ArtifactCommitIntentId,
    authorize_requests: &mut HashMap<u64, RetryRequest<CommitAuthorizeRequest>>,
) -> Result<Option<CommitAuthorizeOutcome>, VoomError> {
    if let Some(request) = authorize_requests.get(&intent_id.0) {
        return Ok(Some(
            context
                .api
                .authorize_commit_intent(intent_id, request)
                .await?,
        ));
    }
    let request = RetryRequest::new(
        new_key("commit-authorize"),
        &CommitAuthorizeRequest {
            node_id: context.node_id,
            incarnation_id: context.incarnation_id,
        },
    )?;
    match context
        .api
        .authorize_commit_intent(intent_id, &request)
        .await
    {
        Ok(outcome) => {
            authorize_requests.insert(intent_id.0, request);
            Ok(Some(outcome))
        }
        Err(VoomError::Conflict(message)) if message.contains("not pending") => Ok(None),
        Err(error) => Err(error),
    }
}

/// Re-observe a `recovery_required` intent read-only and file the typed
/// supplemental receipt: matching target bytes are `applied`, drifting bytes
/// are `mismatched`, and an absent target with no temp sibling is the
/// resolved-not-applied `outcome_unknown` evidence. Nothing is mutated.
async fn drive_recovery_intent(
    context: &CommitCoordinatorContext,
    intent: &OpenCommitIntent,
) -> Result<(), VoomError> {
    let expected = CommitObservedFacts {
        size_bytes: intent.expected_facts.size_bytes,
        content_hash: intent.expected_facts.content_hash.clone(),
    };
    let target_path = resolve_rooted_path(
        context,
        intent.target_storage_root_id,
        &intent.target_provider_relative_locator,
    )
    .await?;
    let evidence = match try_observe_regular_file(&target_path).await? {
        Some(observed) if observed == expected => {
            CommitOutcomeEvidence::Applied(CommitAppliedEvidence { observed })
        }
        Some(observed) => CommitOutcomeEvidence::Mismatched(CommitMismatchedEvidence {
            reason: "target bytes do not match the pinned expected facts".to_owned(),
            observed: Some(observed),
        }),
        None => CommitOutcomeEvidence::OutcomeUnknown(CommitOutcomeUnknownEvidence {
            reason: if temp_sibling_present(&target_path).await? {
                format!("{RESOLVED_NOT_APPLIED_REASON}_variant_target_absent_with_temp_sibling")
            } else {
                RESOLVED_NOT_APPLIED_REASON.to_owned()
            },
        }),
    };
    report_outcome(context, intent.id, evidence).await?;

    Ok(())
}

async fn open_commit_intents(
    context: &CommitCoordinatorContext,
) -> Result<CommitOpenOutcome, VoomError> {
    let request = RetryRequest::new(
        new_key("commit-open"),
        &CommitOpenRequest {
            node_id: context.node_id,
            incarnation_id: context.incarnation_id,
        },
    )?;
    context.api.commit_open(&request).await
}

async fn report_outcome(
    context: &CommitCoordinatorContext,
    intent_id: ArtifactCommitIntentId,
    evidence: CommitOutcomeEvidence,
) -> Result<CommitReceiptOutcome, VoomError> {
    let request = RetryRequest::new(
        new_key("commit-outcome"),
        &CommitOutcomeRequest {
            node_id: context.node_id,
            incarnation_id: context.incarnation_id,
            evidence,
        },
    )?;
    context.api.report_commit_outcome(intent_id, &request).await
}

async fn complete_commit(
    context: &CommitCoordinatorContext,
    intent_id: ArtifactCommitIntentId,
    fence_hex: &str,
) -> Result<CommitCompleteOutcome, VoomError> {
    let request = RetryRequest::new(
        new_key("commit-complete"),
        &CommitCompleteRequest {
            node_id: context.node_id,
            incarnation_id: context.incarnation_id,
            fence_hex: fence_hex.to_owned(),
        },
    )?;
    context
        .api
        .complete_commit_intent(intent_id, &request)
        .await
}

/// Promote staged bytes to the target add-only: observe staging against the
/// pinned facts first (drift mutates nothing), converge on an existing
/// matching target, then copy through a unique temp sibling and hard-link
/// into place no-replace with parent-directory fsyncs.
async fn promote_staged_bytes(
    staging_path: &Path,
    target_path: &Path,
    expected: &CommitObservedFacts,
) -> Result<Promotion, VoomError> {
    let staged = observe_regular_file(staging_path).await?;
    if staged != *expected {
        return Ok(Promotion::Mismatched {
            reason: "staged bytes do not match the pinned expected facts".to_owned(),
            observed: Some(staged),
        });
    }
    if let Some(existing) = try_observe_regular_file(target_path).await? {
        return Ok(if existing == *expected {
            // A prior attempt already promoted matching bytes: converge
            // without rewriting.
            Promotion::Applied(existing)
        } else {
            Promotion::Mismatched {
                reason: "target already exists with different bytes".to_owned(),
                observed: Some(existing),
            }
        });
    }

    let temp_path = unique_temp_sibling_path(target_path)?;
    if let Some(parent) = temp_path.parent() {
        // A fresh library root may not contain the target's directory yet;
        // creating it is idempotent and touches no file bytes.
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            VoomError::ArtifactUnavailable(format!(
                "cannot prepare artifact directory {}: {error}",
                parent.display()
            ))
        })?;
    }

    match copy_to_temp_no_replace(staging_path, &temp_path, expected).await? {
        CopyOutcome::Copied => {}
        CopyOutcome::StagingDrifted(observed) => {
            return Ok(Promotion::Mismatched {
                reason: "staged bytes drifted while copying them".to_owned(),
                observed: Some(observed),
            });
        }
    }
    match install_temp_no_replace(&temp_path, target_path).await {
        Ok(()) => {}
        Err(InstallError::TargetAppeared) => {
            let _ = remove_file_if_exists(&temp_path).await;
            let existing = observe_regular_file(target_path).await?;
            return Ok(if existing == *expected {
                Promotion::Applied(existing)
            } else {
                Promotion::Mismatched {
                    reason: "target appeared concurrently with different bytes".to_owned(),
                    observed: Some(existing),
                }
            });
        }
        Err(InstallError::Failed(error)) => return Err(error),
    }

    let target_facts = observe_regular_file(target_path).await?;
    if target_facts != *expected {
        return Err(VoomError::VerificationFailure(format!(
            "committed target facts do not match verified staged artifact: {}",
            target_path.display()
        )));
    }
    Ok(Promotion::Applied(target_facts))
}

enum CopyOutcome {
    Copied,
    /// The staged bytes drifted between observation and copy. No byte of the
    /// target was touched; the temp sibling is already removed.
    StagingDrifted(CommitObservedFacts),
}

/// Outcome of ensuring staged bytes exist before promotion.
enum Materialize {
    /// Staging already held bytes (a worker's output or an earlier attempt's
    /// copy); they were left untouched and promotion decides their match.
    Present,
    /// Staging was absent and was materialized from the pinned source handle,
    /// the copied facts matching the pinned expectations exactly.
    Materialized,
    /// Staging was absent and the source handle's bytes do not match the
    /// pinned expected facts: typed drift evidence, nothing installed.
    SourceMismatch {
        reason: String,
        observed: CommitObservedFacts,
    },
}

/// Materialize missing staged bytes from the pinned source handle (ADR 0075
/// "staging copy joins the fenced commit intent"): an absent staging file is
/// copied from the source rooted address through a unique temp sibling and
/// installed no-replace, rejecting any copy whose observed facts drift from
/// the pinned expected facts. Existing staged bytes are never overwritten —
/// mutation outputs staged by workers on this node must survive, and their
/// match against the pins is promotion's decision.
async fn materialize_staging_from_source(
    staging_path: &Path,
    source_path: &Path,
    expected: &CommitObservedFacts,
) -> Result<Materialize, VoomError> {
    if try_observe_regular_file(staging_path).await?.is_some() {
        return Ok(Materialize::Present);
    }
    let temp_path = unique_temp_sibling_path(staging_path)?;
    if let Some(parent) = temp_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            VoomError::ArtifactUnavailable(format!(
                "cannot prepare artifact directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    match stream_copy_with_hash(source_path, &temp_path).await {
        Ok(copied) if copied == *expected => {
            match install_temp_no_replace(&temp_path, staging_path).await {
                Ok(()) => Ok(Materialize::Materialized),
                Err(InstallError::TargetAppeared) => {
                    // A concurrent attempt materialized staging first: leave
                    // its bytes in place; promotion re-observes the pins.
                    let _ = remove_file_if_exists(&temp_path).await;
                    Ok(Materialize::Present)
                }
                Err(InstallError::Failed(error)) => Err(error),
            }
        }
        Ok(copied) => {
            let _ = remove_file_if_exists(&temp_path).await;
            Ok(Materialize::SourceMismatch {
                reason: format!(
                    "staged bytes absent and source handle {} does not match the pinned \
                     expected facts",
                    source_path.display()
                ),
                observed: copied,
            })
        }
        Err(error) => {
            let _ = remove_file_if_exists(&temp_path).await;
            Err(error)
        }
    }
}

/// Copy staging into a fresh temp sibling created no-replace, fsync the file,
/// and reject the copy if the staged bytes drifted away from `expected`
/// mid-copy. The temp sibling is removed on every failure path.
async fn copy_to_temp_no_replace(
    staging_path: &Path,
    temp_path: &Path,
    expected: &CommitObservedFacts,
) -> Result<CopyOutcome, VoomError> {
    match stream_copy_with_hash(staging_path, temp_path).await {
        Ok(copied) if copied == *expected => Ok(CopyOutcome::Copied),
        Ok(copied) => {
            let _ = remove_file_if_exists(temp_path).await;
            Ok(CopyOutcome::StagingDrifted(copied))
        }
        Err(error) => {
            let _ = remove_file_if_exists(temp_path).await;
            Err(error)
        }
    }
}

async fn stream_copy_with_hash(
    staging_path: &Path,
    temp_path: &Path,
) -> Result<CommitObservedFacts, VoomError> {
    use tokio::io::AsyncWriteExt;

    let mut source = tokio::fs::File::open(staging_path).await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "cannot read artifact path {}: {error}",
            staging_path.display()
        ))
    })?;
    let mut destination = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
        .await
        .map_err(|error| {
            VoomError::ArtifactUnavailable(format!(
                "cannot create temporary artifact path {}: {error}",
                temp_path.display()
            ))
        })?;
    let mut hasher = blake3::Hasher::new();
    let mut size_bytes = 0_u64;
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
    let write_result = async {
        loop {
            let read = source.read(&mut buffer).await.map_err(|error| {
                VoomError::ArtifactUnavailable(format!(
                    "cannot read artifact path {}: {error}",
                    staging_path.display()
                ))
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            size_bytes += u64::try_from(read).unwrap_or(u64::MAX);
            destination
                .write_all(&buffer[..read])
                .await
                .map_err(|error| {
                    VoomError::ArtifactUnavailable(format!(
                        "cannot write temporary artifact path {}: {error}",
                        temp_path.display()
                    ))
                })?;
        }
        destination.flush().await.map_err(|error| {
            VoomError::ArtifactUnavailable(format!(
                "cannot flush temporary artifact path {}: {error}",
                temp_path.display()
            ))
        })?;
        destination.sync_all().await.map_err(|error| {
            VoomError::CommitFailure(format!(
                "cannot fsync temporary artifact path {}: {error}",
                temp_path.display()
            ))
        })
    }
    .await;
    write_result?;
    Ok(CommitObservedFacts {
        size_bytes,
        content_hash: format!("blake3:{}", hasher.finalize().to_hex()),
    })
}

/// Hard-link the fsynced temp sibling onto the target path without ever
/// replacing an existing name, fsyncing the parent directory around the
/// rename-equivalent. Ported from the retired host-side promoter.
async fn install_temp_no_replace(temp_path: &Path, target_path: &Path) -> Result<(), InstallError> {
    if let Err(error) = tokio::fs::hard_link(temp_path, target_path).await {
        if error.kind() == ErrorKind::AlreadyExists {
            return Err(InstallError::TargetAppeared);
        }
        let _ = remove_file_if_exists(temp_path).await;
        return Err(InstallError::Failed(VoomError::CommitFailure(format!(
            "cannot install artifact {} to {} without replacement: {error}",
            temp_path.display(),
            target_path.display()
        ))));
    }
    if let Err(error) = fsync_parent_dir(target_path).await {
        let _ = remove_file_if_exists(target_path).await;
        return Err(InstallError::Failed(error));
    }
    if let Err(error) = tokio::fs::remove_file(temp_path).await {
        return Err(InstallError::Failed(VoomError::CommitFailure(format!(
            "cannot remove temporary artifact path {} after install: {error}",
            temp_path.display()
        ))));
    }
    fsync_parent_dir(target_path)
        .await
        .map_err(InstallError::Failed)
}

/// Observe a regular file's pinned-fact shape, rejecting symlinks and
/// non-regular files, hashing with blake3 while re-checking for concurrent
/// mutation. An absent path is an error here; use
/// [`try_observe_regular_file`] where absence is meaningful.
async fn observe_regular_file(path: &Path) -> Result<CommitObservedFacts, VoomError> {
    try_observe_regular_file(path).await?.ok_or_else(|| {
        VoomError::ArtifactUnavailable(format!("artifact path is missing: {}", path.display()))
    })
}

/// [`observe_regular_file`], with `Ok(None)` for an absent path.
pub(crate) async fn try_observe_regular_file(
    path: &Path,
) -> Result<Option<CommitObservedFacts>, VoomError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(VoomError::ArtifactUnavailable(format!(
                "cannot inspect artifact path {}: {error}",
                path.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(VoomError::ArtifactUnavailable(format!(
            "artifact path must not be a symlink: {}",
            path.display()
        )));
    }
    let canonical = tokio::fs::canonicalize(path).await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "cannot canonicalize artifact path {}: {error}",
            path.display()
        ))
    })?;
    let mut file = tokio::fs::File::open(&canonical).await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "cannot read artifact path {}: {error}",
            canonical.display()
        ))
    })?;
    let start_metadata = file.metadata().await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "cannot inspect artifact path {}: {error}",
            canonical.display()
        ))
    })?;
    if !start_metadata.is_file() {
        return Err(VoomError::ArtifactUnavailable(format!(
            "artifact path must be a regular file: {}",
            canonical.display()
        )));
    }

    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer).await.map_err(|error| {
            VoomError::ArtifactUnavailable(format!(
                "cannot read artifact path {}: {error}",
                canonical.display()
            ))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let final_metadata = file.metadata().await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "cannot inspect artifact path {} after read: {error}",
            canonical.display()
        ))
    })?;
    if start_metadata.len() != final_metadata.len()
        || start_metadata.modified().ok() != final_metadata.modified().ok()
    {
        return Err(VoomError::ArtifactChecksumMismatch(format!(
            "artifact changed while reading it: {}",
            canonical.display()
        )));
    }
    Ok(Some(CommitObservedFacts {
        size_bytes: final_metadata.len(),
        content_hash: format!("blake3:{}", hasher.finalize().to_hex()),
    }))
}

/// Whether any interrupted-promotion temp sibling sits beside `path`, using
/// the retired promoter's `.voom-tmp.<file>.<pid>.<counter>` naming.
async fn temp_sibling_present(path: &Path) -> Result<bool, VoomError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        VoomError::Config(format!(
            "artifact path must include a file name: {}",
            path.display()
        ))
    })?;
    let mut entries = match tokio::fs::read_dir(parent).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(VoomError::ArtifactUnavailable(format!(
                "cannot list artifact directory {}: {error}",
                parent.display()
            )));
        }
    };
    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "cannot list artifact directory {}: {error}",
            parent.display()
        ))
    })? {
        if is_temp_sibling_of(
            &entry.file_name().to_string_lossy(),
            &file_name.to_string_lossy(),
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Exact match against the retired promoter's naming:
/// `{TEMP_SIBLING_PREFIX}{file_name}.{pid}.{counter}` with numeric pid and
/// counter. Prefix-only matching would misclassify lookalikes — e.g. a
/// `.voom-tmp.data.bin2.…` sibling belongs to `data.bin2`, not `data.bin`.
fn is_temp_sibling_of(entry_name: &str, file_name: &str) -> bool {
    let Some(tail) = entry_name
        .strip_prefix(TEMP_SIBLING_PREFIX)
        .and_then(|rest| rest.strip_prefix(file_name))
        .and_then(|tail| tail.strip_prefix('.'))
    else {
        return false;
    };
    let mut parts = tail.split('.');
    let numeric =
        |value: &str| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(pid), Some(counter), None) => numeric(pid) && numeric(counter),
        _ => false,
    }
}

fn unique_temp_sibling_path(final_path: &Path) -> Result<PathBuf, VoomError> {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = final_path.file_name().ok_or_else(|| {
        VoomError::Config(format!(
            "artifact path must include a file name: {}",
            final_path.display()
        ))
    })?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        "{TEMP_SIBLING_PREFIX}{}.{}.{}",
        file_name.to_string_lossy(),
        std::process::id(),
        counter
    )))
}

/// Shared rooted-path resolution for both commit intents and media dispatches:
/// canonicalize the bound root, validate the relative locator textually, then
/// reject symlinked directory components that could redirect outside the root.
pub(crate) async fn rooted_path(
    storage_roots: &HashMap<u64, PathBuf>,
    storage_root_id: StorageRootId,
    relative_locator: &str,
) -> Result<PathBuf, VoomError> {
    let relative = Path::new(relative_locator);
    if relative_locator.is_empty()
        || relative.is_absolute()
        || relative_locator.contains('\\')
        || relative
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(VoomError::Config(format!(
            "storage root {storage_root_id} locator {relative_locator:?} must be a relative path \
             with no '..', backslash, or empty components"
        )));
    }
    let root_path = canonical_root(storage_roots, storage_root_id).await?;
    let resolved = root_path.join(relative);
    if !resolved.starts_with(&root_path) {
        return Err(VoomError::Config(format!(
            "storage root {storage_root_id} locator {relative_locator:?} escapes the root at {}",
            root_path.display()
        )));
    }
    // A symlinked directory component could redirect the sink outside the
    // root even though the textual locator stays inside it. Every component
    // except the leaf (which may legitimately not exist yet) must be a real
    // directory inside the canonical root.
    let components: Vec<_> = relative.components().collect();
    let mut walked = root_path.clone();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        walked.push(component);
        match tokio::fs::symlink_metadata(&walked).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(VoomError::Config(format!(
                    "storage root {storage_root_id} locator {relative_locator:?} traverses \
                     symlink {}",
                    walked.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(VoomError::ArtifactUnavailable(format!(
                    "cannot inspect artifact path component {}: {error}",
                    walked.display()
                )));
            }
        }
    }
    Ok(resolved)
}

/// Canonicalized provider locator for one bound storage root.
pub(crate) async fn canonical_root(
    storage_roots: &HashMap<u64, PathBuf>,
    storage_root_id: StorageRootId,
) -> Result<PathBuf, VoomError> {
    let locator = storage_roots.get(&storage_root_id.0).ok_or_else(|| {
        VoomError::Config(format!(
            "no provider locator configured for storage root {storage_root_id}"
        ))
    })?;
    tokio::fs::canonicalize(locator).await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "cannot resolve storage root {storage_root_id} at {}: {error}",
            locator.display()
        ))
    })
}

async fn resolve_rooted_path(
    context: &CommitCoordinatorContext,
    storage_root_id: StorageRootId,
    relative_locator: &str,
) -> Result<PathBuf, VoomError> {
    rooted_path(&context.storage_roots, storage_root_id, relative_locator).await
}

pub(crate) async fn remove_file_if_exists(path: &Path) -> Result<(), VoomError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VoomError::CommitFailure(format!(
            "cannot remove artifact path {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(unix)]
async fn fsync_parent_dir(path: &Path) -> Result<(), VoomError> {
    let parent = path.parent().unwrap_or_else(|| Path::new(".")).to_owned();
    tokio::task::spawn_blocking(move || {
        std::fs::File::open(&parent)
            .and_then(|file| file.sync_all())
            .map_err(|err| {
                VoomError::CommitFailure(format!(
                    "cannot fsync artifact parent directory {}: {err}",
                    parent.display()
                ))
            })
    })
    .await
    .map_err(|err| VoomError::Internal(format!("artifact directory fsync task failed: {err}")))?
}

#[cfg(not(unix))]
async fn fsync_parent_dir(_path: &Path) -> Result<(), VoomError> {
    Ok(())
}

#[cfg(test)]
#[path = "commit_test.rs"]
mod tests;
