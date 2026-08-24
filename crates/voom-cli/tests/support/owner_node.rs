//! Owner-node emulation for CLI e2e suites (ADR 0075 / issue #423 T9).
//!
//! Envelope-bearing media tickets are never leased by the workflow executor:
//! they wait `ready` for their storage owner's agent. These suites have no
//! real agent, so background threads stand in for one. For every ready
//! media-dispatch ticket the emulator
//!
//! 1. strict-decodes the envelope exactly like `voom-node-agent` does,
//! 2. performs the byte work against its own storage roots (deterministic
//!    synthetic bytes for staged outputs),
//! 3. records the durable artifact chain — handle, staging location,
//!    succeeded verification, fenced commit intent driven to convergence by
//!    a [`SimulatedOwnerNode`], and the produced version's reprobe snapshot —
//!    through the same public control-plane case functions an operator or
//!    node would drive,
//! 4. and releases the ticket `succeeded` with the committed-result payload
//!    the coordinator's finalize leg consumes.
//!
//! The durable lease/commit truth this produces is real: real rows, real
//! bytes on disk, real no-replace promotion. Only the child media process is
//! synthetic — exactly the surface the deleted bundled pipeline used to own.

#![expect(
    clippy::unwrap_used,
    reason = "background emulator threads cannot propagate Results; a failed \
              settlement must fail loudly so the driving test fails"
)]
#![expect(
    clippy::panic,
    reason = "the emulator runs on background threads; a settlement failure \
              must abort loudly instead of being swallowed by the loop"
)]

use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use voom_control_plane::artifact::VerifyArtifactInput;
use voom_control_plane::artifact_commit::CommitArtifactInput;
use voom_control_plane::{ControlPlane, workers::RegisterWorkerInput};
use voom_core::VoomError;
use voom_core::ids::{
    ArtifactCommitRecordId, ArtifactHandleId, ArtifactLocationId, ArtifactVerificationId,
    FileLocationId, FileVersionId, MediaSnapshotId,
};
use voom_store::repo::media::artifacts::{
    ArtifactHandleAccessMode, ArtifactLocationKind, NewArtifactHandle, NewArtifactLocation,
    SqliteArtifactRepo,
};
use voom_test_support::commit_node::SimulatedOwnerNode;
use voom_worker_protocol::{
    MediaDispatch, MediaPlannedOutput, MediaProbeDispatch, decode_media_dispatch,
};
/// How often the emulator loops poll for work.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// The worker row the emulator records its released settlement leases against.
const EMULATOR_WORKER_NAME: &str = "owner-node-media-emulator";

/// A running pair of background threads (commit-intent driver + media
/// settlement). Kept alive for the test's duration.
#[derive(Debug)]
pub struct OwnerNodeEmulator {
    _handles: Vec<std::thread::JoinHandle<()>>,
}

impl OwnerNodeEmulator {
    /// Install the simulated owner principal and spawn both emulator loops
    /// against `url`.
    #[must_use]
    pub fn spawn(url: &str) -> Self {
        let commit_url = url.to_owned();
        let media_url = commit_url.clone();
        let commit_handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let pool = voom_store::connect(&commit_url).await.unwrap();
                let node = SimulatedOwnerNode::new().unwrap();
                node.install_principal(&pool).await.unwrap();
                let cp = ControlPlane::open(&commit_url)
                    .await
                    .unwrap()
                    .with_local_node_id(Some(voom_core::NodeId(
                        voom_store::test_support::TEST_STORAGE_ROOT_ID.0,
                    )));
                activate_owner_media_workers(&cp, &node).await.unwrap();
                loop {
                    let pending: Option<(i64, i64)> = sqlx::query_as(
                        "SELECT id, artifact_handle_id FROM artifact_commit_intents \
                         WHERE state = 'pending' ORDER BY id ASC LIMIT 1",
                    )
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
                    if let Some((_, handle)) = pending {
                        let _ = node
                            .drive_pending_commit(
                                &cp,
                                &pool,
                                voom_core::ArtifactHandleId(u64::try_from(handle).unwrap()),
                            )
                            .await;
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            });
        });
        let media_handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(run_media_settlement(media_url));
        });
        Self {
            _handles: vec![commit_handle, media_handle],
        }
    }
}

/// Poll for and settle ready envelope-bearing media tickets until the process
/// ends.
async fn run_media_settlement(url: String) {
    let pool = voom_store::connect(&url).await.unwrap();
    let cp =
        ControlPlane::open_with_pool(pool.clone(), std::sync::Arc::new(voom_core::SystemClock))
            .await
            .unwrap()
            .with_local_node_id(Some(voom_core::NodeId(
                voom_store::test_support::TEST_STORAGE_ROOT_ID.0,
            )));
    ensure_emulator_worker(&cp, &pool).await.unwrap();

    let mut tick = tokio::time::interval(POLL_INTERVAL);
    tick.tick().await;
    loop {
        tick.tick().await;
        let Ok(ready) = sqlx::query_as::<_, (i64,)>(
            "SELECT id FROM tickets \
             WHERE state = 'ready' \
               AND json_extract(payload, '$.rendered_payload.media_dispatch') IS NOT NULL \
             ORDER BY id ASC LIMIT 1",
        )
        .fetch_all(&pool)
        .await
        else {
            continue;
        };
        for (ticket_id,) in ready {
            if let Err(error) = settle_ticket(&cp, &pool, ticket_id).await {
                panic!("owner-node emulator failed to settle ticket {ticket_id}: {error}");
            }
        }
    }
}

async fn ensure_emulator_worker(cp: &ControlPlane, pool: &SqlitePool) -> Result<(), VoomError> {
    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM workers WHERE name = ?")
        .bind(EMULATOR_WORKER_NAME)
        .fetch_optional(pool)
        .await
        .map_err(|error| db(&error))?;
    if existing.is_none() {
        cp.register_worker(RegisterWorkerInput {
            name: EMULATOR_WORKER_NAME.to_owned(),
            kind: voom_core::WorkerKind::Synthetic,
        })
        .await?;
    }
    Ok(())
}

/// Activate the owner principal's manifest with one declared worker per media
/// tool, exactly as a real node agent's activation does (ADR 0076): the
/// resulting remote workers satisfy owner-scoped `requires_tools` preflight.
async fn activate_owner_media_workers(
    cp: &ControlPlane,
    node: &SimulatedOwnerNode,
) -> Result<(), VoomError> {
    use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
    use voom_core::{ArtifactAccessMode, OperationKind};
    let declarations = [
        (
            "ffmpeg",
            vec![
                OperationKind::TranscodeVideo,
                OperationKind::TranscodeAudio,
                OperationKind::ExtractAudio,
            ],
        ),
        ("mkvtoolnix", vec![OperationKind::Remux]),
        ("ffprobe", vec![OperationKind::ProbeFile]),
    ]
    .into_iter()
    .map(|(logical_name, operations)| RemoteWorkerDeclaration {
        logical_name: logical_name.to_owned(),
        operations,
        artifact_access: vec![ArtifactAccessMode::SharedMount],
        max_parallel: 2,
    })
    .collect();
    // One authenticated agent incarnation both supervises these workers and
    // drives the commit-intent protocol below. Activating a second incarnation
    // would supersede the principal used by `drive_pending_commit`.
    let incarnation_id = node.incarnation_id;
    cp.remote_activate(RemoteActivateInput {
        node_id: node.node_id,
        token: node.token.clone(),
        idempotency_key: "activate-owner-media-workers".to_owned(),
        request_hash: "activate-owner-media-workers-body".to_owned(),
        incarnation_id,
        workers: declarations,
    })
    .await
    .map(|_| ())
}

/// Map a raw sqlx error into the crate error for `?` plumbing.
fn db(error: &sqlx::Error) -> VoomError {
    VoomError::database(error.to_string())
}

/// A ready ticket's decoded envelope plus the fields settlement consumes.
struct ReadyTicket {
    job_id: Option<i64>,
    rendered: Value,
    envelope: MediaDispatch,
}
/// Load a `ready` ticket and strict-decode its media-dispatch envelope.
async fn ready_ticket(pool: &SqlitePool, ticket_id: i64) -> Result<ReadyTicket, VoomError> {
    let (job_id, payload): (Option<i64>, String) =
        sqlx::query_as("SELECT job_id, payload FROM tickets WHERE id = ? AND state = 'ready'")
            .bind(ticket_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| db(&error))?
            .ok_or_else(|| VoomError::NotFound(format!("ready ticket {ticket_id}")))?;
    let payload: Value = serde_json::from_str(&payload)
        .map_err(|error| VoomError::Internal(format!("ticket {ticket_id} payload: {error}")))?;
    let rendered = payload.get("rendered_payload").ok_or_else(|| {
        VoomError::Internal(format!("ticket {ticket_id} has no rendered payload"))
    })?;
    let envelope_payload = rendered
        .get("media_dispatch")
        .ok_or_else(|| VoomError::Internal(format!("ticket {ticket_id} has no envelope")))?;
    let envelope = decode_media_dispatch(envelope_payload)
        .map_err(|reason| VoomError::Internal(format!("ticket {ticket_id} envelope: {reason}")))?;
    Ok(ReadyTicket {
        job_id,
        rendered: rendered.clone(),
        envelope,
    })
}

/// The rebuilt CLI suites never produce these envelopes.
fn unsupported_envelope(operation: &str) -> VoomError {
    VoomError::Internal(format!(
        "owner-node emulator: {operation} settlement is not exercised by the \
         rebuilt CLI e2e suites"
    ))
}

/// Perform one ticket's byte work and release it `succeeded`.
async fn settle_ticket(
    cp: &ControlPlane,
    pool: &SqlitePool,
    ticket_id: i64,
) -> Result<(), VoomError> {
    let ticket = ready_ticket(pool, ticket_id).await?;
    let rendered = &ticket.rendered;
    let result = match &ticket.envelope {
        MediaDispatch::Probe(dispatch) => json!({
            "codec": rendered.get("codec").and_then(Value::as_str).unwrap_or("h264"),
            "agent_observed": probe_observation(dispatch),
        }),
        MediaDispatch::TranscodeVideo(dispatch) => {
            settle_staged_output(&StagedOutputSettlement {
                cp,
                pool,
                ticket_id,
                job_id: ticket.job_id,
                rendered,
                operation_dir: "transcode",
                output: &dispatch.output,
                output_codec: dispatch.output_video_codec.as_str(),
                audio_family: false,
                rewrite_audio_streams: false,
            })
            .await?
        }
        MediaDispatch::Remux(dispatch) => {
            settle_staged_output(&StagedOutputSettlement {
                cp,
                pool,
                ticket_id,
                job_id: ticket.job_id,
                rendered,
                operation_dir: "remux",
                output: &dispatch.output,
                output_codec: "copy",
                audio_family: false,
                rewrite_audio_streams: false,
            })
            .await?
        }
        MediaDispatch::TranscodeAudio(dispatch) => {
            settle_staged_output(&StagedOutputSettlement {
                cp,
                pool,
                ticket_id,
                job_id: ticket.job_id,
                rendered,
                operation_dir: "audio",
                output: &dispatch.output,
                output_codec: dispatch.settings.target_codec.as_str(),
                audio_family: false,
                // Replace-in-place audio transcodes really do re-encode every
                // selected stream; synthesized companions only add a track.
                rewrite_audio_streams: !dispatch.settings.add_track,
            })
            .await?
        }
        MediaDispatch::ExtractAudio(dispatch) => {
            // One synthetic sidecar per planned extraction output.
            let first = dispatch.outputs.first().ok_or_else(|| {
                VoomError::Internal(format!("ticket {ticket_id} extracts no outputs"))
            })?;
            let mut result = settle_staged_output(&StagedOutputSettlement {
                cp,
                pool,
                ticket_id,
                job_id: ticket.job_id,
                rendered,
                operation_dir: "audio",
                output: &first.output,
                output_codec: dispatch.output_container.as_str(),
                audio_family: true,
                rewrite_audio_streams: false,
            })
            .await?;
            strip_rich_fields(&mut result);
            result
        }
        MediaDispatch::BackUpFile(_) => return Err(unsupported_envelope("back_up_file")),
        MediaDispatch::VerifyArtifact(_) => return Err(unsupported_envelope("verify_artifact")),
    };

    release_succeeded(pool, ticket_id, &result).await
}

/// Everything one staged-output settlement needs, grouped so the byte chain
/// stays readable.
struct StagedOutputSettlement<'a> {
    cp: &'a ControlPlane,
    pool: &'a SqlitePool,
    ticket_id: i64,
    job_id: Option<i64>,
    rendered: &'a Value,
    operation_dir: &'a str,
    output: &'a MediaPlannedOutput,
    output_codec: &'a str,
    /// Extraction-family results carry a narrower legacy schema.
    audio_family: bool,
    /// Replace-in-place audio transcodes re-encode every audio stream.
    rewrite_audio_streams: bool,
}

/// Durable staged-output rows plus the facts of the emulated bytes.
struct StagedArtifact {
    root: PathBuf,
    staging_path: PathBuf,
    handle_id: ArtifactHandleId,
    location_id: ArtifactLocationId,
    verification_id: ArtifactVerificationId,
    size_bytes: u64,
    checksum: String,
}

/// Durable outcome of committing a staged output into its terminal layout.
struct CommittedOutput {
    commit_record_id: ArtifactCommitRecordId,
    result_file_version_id: FileVersionId,
    result_file_location_id: FileLocationId,
    target_path: PathBuf,
}

/// The recorded reprobe snapshot for a produced version.
struct RecordedReprobe {
    snapshot_id: MediaSnapshotId,
    payload: Value,
}

impl StagedOutputSettlement<'_> {
    /// Source identity the envelope pins for this operation.
    fn source_ids(&self) -> Result<(u64, u64), VoomError> {
        let source_file_version_id = u64_from_payload(
            self.rendered
                .get("source_file_version_id")
                .and_then(Value::as_u64),
            self.ticket_id,
            "source_file_version_id",
        )?;
        let source_file_location_id = u64_from_payload(
            self.rendered
                .get("source_location_id")
                .and_then(Value::as_u64),
            self.ticket_id,
            "source_location_id",
        )?;
        Ok((source_file_version_id, source_file_location_id))
    }

    /// Facts and durable rows for one emulated staged output: synthetic bytes
    /// on disk, a handle describing them, their staging location, and a
    /// succeeded verification. Handle facts describe the OUTPUT bytes; the
    /// pinned file version is the operation's source.
    async fn stage_and_verify(&self) -> Result<StagedArtifact, VoomError> {
        let (source_file_version_id, source_file_location_id) = self.source_ids()?;
        let root = root_path(self.pool, self.output.storage_root_id.0).await?;
        let staging_path = root.join(self.output.provider_relative_locator.as_str());
        write_synthetic_output(&staging_path, self.output_codec)?;

        let bytes = tokio::fs::read(&staging_path).await.map_err(|error| {
            VoomError::ArtifactUnavailable(format!(
                "read emulated output {}: {error}",
                staging_path.display()
            ))
        })?;
        let size_bytes = u64::try_from(bytes.len())
            .map_err(|_| VoomError::Internal("emulated output size overflow".to_owned()))?;
        let checksum = format!("blake3:{}", blake3::hash(&bytes).to_hex());

        let now = time::OffsetDateTime::now_utc();
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| db(&error))?;
        let artifacts = SqliteArtifactRepo::new(self.pool.clone());
        let handle = artifacts
            .create_handle_in_tx(
                &mut tx,
                NewArtifactHandle {
                    size_bytes: Some(i64::try_from(size_bytes).map_err(|_| {
                        VoomError::Internal("emulated output exceeds SQLite integer".to_owned())
                    })?),
                    checksum: Some(checksum.clone()),
                    privacy_class: "internal".to_owned(),
                    durability_class: "staging".to_owned(),
                    allowed_access_modes: vec![ArtifactHandleAccessMode::LocalPath],
                    mutability: "immutable".to_owned(),
                    // The commit prepare leg reads its pinned source handle
                    // back out of this lineage block
                    // (`$.source_file_location_id`).
                    source_lineage: Some(json!({
                        "source_file_version_id": source_file_version_id,
                        "source_file_location_id": source_file_location_id,
                    })),
                    file_version_id: Some(voom_core::FileVersionId(source_file_version_id)),
                    created_at: now,
                },
            )
            .await?;
        let location = artifacts
            .record_location_in_tx(
                &mut tx,
                NewArtifactLocation {
                    artifact_handle_id: handle.id,
                    kind: ArtifactLocationKind::Staging,
                    value: staging_path.display().to_string(),
                    observed_at: now,
                },
            )
            .await?;
        tx.commit().await.map_err(|error| db(&error))?;

        let verification = self
            .cp
            .verify_artifact(VerifyArtifactInput {
                artifact_handle_id: handle.id,
                staging_root: root.clone(),
            })
            .await?;

        Ok(StagedArtifact {
            root,
            staging_path,
            handle_id: handle.id,
            location_id: location.id,
            verification_id: verification.verification_id,
            size_bytes,
            checksum,
        })
    }

    /// Commit the staged output into the per-operation working dir under the
    /// storage root — the layout the coordinator's promotion plan matches
    /// terminal artifacts against.
    async fn commit(&self, artifact: &StagedArtifact) -> Result<CommittedOutput, VoomError> {
        let file_name = artifact
            .staging_path
            .file_name()
            .map(std::ffi::OsStr::to_string_lossy)
            .ok_or_else(|| VoomError::Internal("staged output has no file name".to_owned()))?;
        let target_name = format!("t{}-{file_name}", self.ticket_id);
        let target_path = artifact
            .root
            .join(".committed")
            .join(self.operation_dir)
            .join(target_name);
        if let Some(parent) = target_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                VoomError::ArtifactUnavailable(format!("create {}: {error}", parent.display()))
            })?;
        }
        let report = self
            .cp
            .commit_artifact(CommitArtifactInput {
                artifact_handle_id: artifact.handle_id,
                target_path: target_path.clone(),
            })
            .await
            .map_err(|error| VoomError::CommitFailure(error.to_string()))?;
        Ok(CommittedOutput {
            commit_record_id: report.commit_record_id,
            result_file_version_id: report.result_file_version_id.ok_or_else(|| {
                VoomError::CommitFailure("committed record carries no result version".to_owned())
            })?,
            result_file_location_id: report.result_file_location_id.ok_or_else(|| {
                VoomError::CommitFailure("committed record carries no result location".to_owned())
            })?,
            target_path,
        })
    }

    /// Reprobe the produced version: a fresh normalized snapshot inheriting
    /// the source's stream layout with the planned facts updated. Video
    /// transcodes rewrite the video stream; replace-in-place audio transcodes
    /// re-encode every audio stream to the target codec (what the real ffmpeg
    /// invocation produces), so downstream phases replan against truthful
    /// observed state.
    async fn record_reprobe(
        &self,
        produced: FileVersionId,
        source_file_version_id: u64,
    ) -> Result<RecordedReprobe, VoomError> {
        let source_payload: Option<String> = sqlx::query_scalar(
            "SELECT payload FROM media_snapshots WHERE file_version_id = ? \
             ORDER BY id DESC LIMIT 1",
        )
        .bind(
            i64::try_from(source_file_version_id)
                .map_err(|e| VoomError::database(e.to_string()))?,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(|error| db(&error))?;
        let mut payload =
            match source_payload.map(|payload| serde_json::from_str::<Value>(&payload)) {
                Some(Ok(payload)) => payload,
                _ => reprobe_snapshot(self.output_codec),
            };
        if self.rewrite_audio_streams {
            if let Some(streams) = payload.get_mut("streams").and_then(Value::as_array_mut) {
                for stream in streams
                    .iter_mut()
                    .filter(|stream| stream.get("kind").and_then(Value::as_str) == Some("audio"))
                {
                    stream["codec_name"] = json!(self.output_codec);
                }
            }
        } else if let Some(streams) = payload.get_mut("streams").and_then(Value::as_array_mut)
            && let Some(video) = streams
                .iter_mut()
                .find(|stream| stream.get("kind").and_then(Value::as_str) == Some("video"))
        {
            video["codec_name"] = json!(self.output_codec);
            video["pixel_format"] = json!("yuv420p");
            video["profile"] = json!("main");
        }
        let snapshot = self
            .cp
            .record_media_snapshot(
                produced,
                None,
                payload.clone(),
                time::OffsetDateTime::now_utc(),
            )
            .await?;
        Ok(RecordedReprobe {
            snapshot_id: snapshot.id,
            payload,
        })
    }

    /// The live source location an extraction result must name: the reprobe
    /// payload carries it when the seeded snapshot did, else the version's
    /// latest live location row wins.
    async fn audio_source_location_id(
        &self,
        source_file_version_id: u64,
        reprobe_payload: &Value,
    ) -> Result<u64, VoomError> {
        if let Some(id) = reprobe_payload
            .get("source_location_id")
            .and_then(Value::as_u64)
        {
            return Ok(id);
        }
        let id: i64 = sqlx::query_scalar(
            "SELECT id FROM file_locations WHERE file_version_id = ? \
             AND retired_at IS NULL ORDER BY id DESC LIMIT 1",
        )
        .bind(
            i64::try_from(source_file_version_id)
                .map_err(|e| VoomError::database(e.to_string()))?,
        )
        .fetch_one(self.pool)
        .await
        .map_err(|error| db(&error))?;
        u64::try_from(id).map_err(|e| VoomError::database(e.to_string()))
    }
}

/// Full mutate → verify → commit chain for one staged-output operation.
async fn settle_staged_output(request: &StagedOutputSettlement<'_>) -> Result<Value, VoomError> {
    let (source_file_version_id, _) = request.source_ids()?;
    let artifact = request.stage_and_verify().await?;
    let committed = request.commit(&artifact).await?;
    let reprobe = request
        .record_reprobe(committed.result_file_version_id, source_file_version_id)
        .await?;

    let mut result = json!({
        "job_id": request.job_id,
        "ticket_id": request.ticket_id,
        "source_file_version_id": source_file_version_id,
        "staged_artifact_handle_id": artifact.handle_id.0,
        "staged_artifact_location_id": artifact.location_id.0,
        "verification_id": artifact.verification_id.0,
        "commit_record_id": committed.commit_record_id.0,
        "result_file_version_id": committed.result_file_version_id.0,
        "result_file_location_id": committed.result_file_location_id.0,
        "output_path": request.output.provider_relative_locator.as_str(),
    });
    if request.audio_family {
        // The legacy audio-extraction result schema is `deny_unknown_fields`:
        // exactly the committed chain plus the two paths, nothing else.
        let location_id = request
            .audio_source_location_id(source_file_version_id, &reprobe.payload)
            .await?;
        result["source_file_location_id"] = json!(location_id);
        result["staging_path"] = json!(artifact.staging_path.display().to_string());
        result["target_path"] = json!(committed.target_path.display().to_string());
    } else {
        result["result_media_snapshot_id"] = json!(reprobe.snapshot_id.0);
        result["agent_observed"] = json!({
            "outputs": [{
                "provider_relative_locator": request.output.provider_relative_locator.as_str(),
                "facts": { "size_bytes": artifact.size_bytes, "content_hash": artifact.checksum },
            }],
        });
    }
    Ok(result)
}

/// Drop the rich-result-only fields an audio-family ticket must not carry.
fn strip_rich_fields(result: &mut Value) {
    if let Some(object) = result.as_object_mut() {
        object.remove("result_media_snapshot_id");
        object.remove("agent_observed");
        object.remove("output_path");
    }
}

fn probe_observation(_dispatch: &MediaProbeDispatch) -> Value {
    json!({})
}

/// Flip the ticket `succeeded` with `result`, recording the released lease the
/// committed-evidence validation joins against.
async fn release_succeeded(
    pool: &SqlitePool,
    ticket_id: i64,
    result: &Value,
) -> Result<(), VoomError> {
    let now = time::OffsetDateTime::now_utc();
    let acquired = iso(&(now - time::Duration::seconds(1)));
    let expires = iso(&(now + time::Duration::seconds(300)));
    let released = iso(&now);
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| db(&error))?;
    sqlx::query(
        "INSERT INTO leases \
         (ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
          ttl_seconds, release_reason, released_at) \
         SELECT ?, id, 'released', ?, ?, ?, 300, 'completed', ? \
         FROM workers WHERE name = ?",
    )
    .bind(ticket_id)
    .bind(&acquired)
    .bind(&expires)
    .bind(&acquired)
    .bind(&released)
    .bind(EMULATOR_WORKER_NAME)
    .execute(&mut *tx)
    .await
    .map_err(|error| db(&error))?;
    let lease_id: i64 =
        sqlx::query_scalar("SELECT id FROM leases WHERE ticket_id = ? ORDER BY id DESC LIMIT 1")
            .bind(ticket_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| db(&error))?;
    let mut result = result.clone();
    result["lease_id"] = json!(lease_id);
    sqlx::query(
        "UPDATE tickets SET state = 'succeeded', result = ?, \
         state_changed_at = ?, epoch = epoch + 1 WHERE id = ? AND state = 'ready'",
    )
    .bind(result.to_string())
    .bind(&released)
    .bind(ticket_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| db(&error))?;
    tx.commit().await.map_err(|error| db(&error))
}

// --- fixture helpers ---

fn iso(at: &time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

async fn root_path(pool: &SqlitePool, root_id: u64) -> Result<PathBuf, VoomError> {
    let locator: String =
        sqlx::query_scalar("SELECT provider_locator FROM library_roots WHERE id = ?")
            .bind(i64::try_from(root_id).map_err(|error| VoomError::database(error.to_string()))?)
            .fetch_optional(pool)
            .await
            .map_err(|error| db(&error))?
            .ok_or_else(|| VoomError::NotFound(format!("library_roots {root_id}")))?;
    tokio::fs::canonicalize(&locator).await.map_err(|error| {
        VoomError::ArtifactUnavailable(format!(
            "cannot resolve storage root {root_id} at {locator}: {error}"
        ))
    })
}

/// Deterministic pseudo-media bytes so facts stay stable across retries.
fn write_synthetic_output(path: &Path, codec: &str) -> Result<(), VoomError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            VoomError::ArtifactUnavailable(format!("create {}: {error}", parent.display()))
        })?;
    }
    let body = format!("voom owner-node emulated {codec} output\n");
    std::fs::write(path, body.as_bytes()).map_err(|error| {
        VoomError::ArtifactUnavailable(format!("write {}: {error}", path.display()))
    })
}

/// Normalized post-commit reprobe snapshot for the produced version.
fn reprobe_snapshot(codec: &str) -> Value {
    json!({
        "format": "sprint10-v1",
        "container": { "format_name": "matroska,webm" },
        "streams": [{
            "id": "stream-0",
            "index": 0,
            "kind": "video",
            "codec_name": codec,
            "pixel_format": "yuv420p",
            "profile": "main",
            "width": 32,
            "height": 32,
            "disposition": { "default": true, "forced": false, "commentary": false },
        }],
    })
}

fn u64_from_payload(value: Option<u64>, ticket_id: i64, field: &str) -> Result<u64, VoomError> {
    value
        .filter(|id| *id > 0)
        .ok_or_else(|| VoomError::Internal(format!("ticket {ticket_id} payload pins no {field}")))
}
