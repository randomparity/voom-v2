use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use time::OffsetDateTime;
use voom_core::{
    ArtifactAccessMode, FileVersionId, NodeId, OperationKind, TicketOperation, VoomError, WorkerId,
};
use voom_policy::{
    CompiledPolicy, DiagnosticSeverity, DiagnosticStage, PolicyDiagnostic, PolicyTool,
    SourceLocation, SourceSpan, compile_policy,
};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::execution::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_store::repo::media::identity::{DiscoveredFile, IngestOutcome};

use super::{PolicyToolTarget, UnavailableTool, format_unavailable_tools, guidance};
use crate::cases::cp;
use crate::cases::execution::remote_execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use crate::cases::workers::nodes::RegisterNodeInput;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn next_seq() -> usize {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

#[test]
fn unavailable_tools_are_reported_in_observation_order_with_guidance() {
    let message = format_unavailable_tools(
        "published",
        &[
            UnavailableTool {
                subject: "mkvtoolnix".to_owned(),
                reason: "denied".to_owned(),
                guidance: guidance(PolicyTool::Mkvtoolnix),
            },
            UnavailableTool {
                subject: "ffmpeg".to_owned(),
                reason: "stale".to_owned(),
                guidance: guidance(PolicyTool::Ffmpeg),
            },
        ],
    );

    assert_eq!(
        message,
        "tool requirement preflight failed for policy `published`:\n\
         - mkvtoolnix: denied; run a node agent with an mkvtoolnix worker on this storage \
         owner (voom agent documentation)\n\
         - ffmpeg: stale; run a node agent with an ffmpeg worker on this storage owner \
         (voom agent documentation)"
    );
}

#[test]
fn videotoolbox_profiles_do_not_require_a_software_worker() {
    let policy = compile_policy(
        "policy \"videotoolbox\" { \
         phase encode { transcode video to hevc { \
         encoder: hevc_videotoolbox bitrate_kbps: 8000 preset: default \
         codec_profile: main pixel_format: yuv420p decode: video_toolbox } } }",
    )
    .unwrap()
    .policy;

    let requirements = super::policy_video_backend_requirements(&policy).unwrap();

    assert!(!requirements.software);
    assert!(!requirements.nvidia.required);
    assert!(requirements.videotoolbox.hardware_decode);
    assert!(
        requirements
            .videotoolbox_encoders
            .contains("hevc_videotoolbox")
    );
}

/// Acceptance criterion 1: a healthy remote owner satisfies all three tokens
/// through the same durable readiness facts every other path uses.
#[tokio::test]
async fn a_healthy_remote_owner_satisfies_ffmpeg_ffprobe_and_mkvtoolnix() {
    let (cp, _tmp) = cp().await;
    let owner = activate_owner_node(
        &cp,
        "healthy-media-node",
        &[
            ("ffmpeg", &[OperationKind::TranscodeVideo]),
            ("mkvtoolnix", &[OperationKind::Remux]),
            ("ffprobe", &[OperationKind::ProbeFile]),
        ],
    )
    .await;
    let file_version_id = owned_root_with_file(&cp, owner.node_id).await;

    cp.preflight_policy_tools(
        &mut policy_requiring(&["ffmpeg", "ffprobe", "mkvtoolnix"]),
        &one_target(file_version_id),
    )
    .await
    .unwrap();
}

/// Acceptance criterion 2: ownership decides. A fully healthy node contributes
/// nothing to a target whose storage lives on another node.
#[tokio::test]
async fn a_healthy_worker_on_a_different_node_does_not_satisfy_the_target() {
    let (cp, _tmp) = cp().await;
    let _healthy = activate_owner_node(
        &cp,
        "healthy-other-node",
        &[
            ("ffmpeg", &[OperationKind::TranscodeVideo]),
            ("ffprobe", &[OperationKind::ProbeFile]),
            ("mkvtoolnix", &[OperationKind::Remux]),
        ],
    )
    .await;
    let starved = activate_owner_node(
        &cp,
        "starved-target-node",
        &[("scanner", &[OperationKind::ScanLibrary])],
    )
    .await;
    let file_version_id = owned_root_with_file(&cp, starved.node_id).await;

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg"]),
            &one_target(file_version_id),
        )
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("on node \"starved-target-node\""),
        "{message}"
    );
    assert!(
        !message.contains("healthy-other-node"),
        "the healthy node leaked into the diagnostic: {message}"
    );
}

/// Hardware readiness is part of the same owner-scoped ffmpeg observation:
/// a suitable accelerator on another active node cannot satisfy this target.
#[tokio::test]
async fn video_hardware_on_a_different_node_does_not_satisfy_the_target() {
    let (cp, _tmp) = cp().await;
    let other = activate_owner_node(
        &cp,
        "vaapi-other-node",
        &[("ffmpeg", &[OperationKind::TranscodeVideo])],
    )
    .await;
    cp.record_capability(NewCapability {
        worker_id: other.workers[0].worker_id,
        operation: TicketOperation::from(OperationKind::TranscodeVideo),
        codecs: Vec::new(),
        hardware: vec!["vaapi:pci-0000:f4:00.0".to_owned()],
        artifact_access: vec![ArtifactAccessMode::SharedMount.as_str().to_owned()],
        extra: vaapi_accelerator_extra(&["hevc_vaapi"], &["hevc"]),
    })
    .await
    .unwrap();
    let target = activate_owner_node(
        &cp,
        "software-target-node",
        &[("ffmpeg", &[OperationKind::TranscodeVideo])],
    )
    .await;
    let file_version_id = owned_root_with_file(&cp, target.node_id).await;

    let error = cp
        .preflight_policy_tools(&mut vaapi_policy(""), &one_target(file_version_id))
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("software-target-node"), "{message}");
    assert!(!message.contains("voom worker run-local"), "{message}");
    assert!(
        message.contains("remote accelerator descriptors are not supported"),
        "{message}"
    );
}

/// The retired run-local providers keep perfect eligibility rows, yet they own
/// no storage and cannot serve any target after ADR 0075.
#[tokio::test]
async fn supervisor_owned_run_local_providers_do_not_satisfy_owned_targets() {
    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::from(OperationKind::TranscodeVideo);
    let local = cp
        .register_supervisor_worker(NewWorker {
            name: "local-ffmpeg-still-running".to_owned(),
            kind: WorkerKind::Local,
            registered_at: cp.clock().now(),
            node_id: None,
        })
        .await
        .unwrap();
    cp.record_capability(NewCapability {
        worker_id: local.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra: json!({}),
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: local.id,
        can_execute: vec![operation],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: json!({}),
    })
    .await
    .unwrap();
    let scanner_only = activate_owner_node(
        &cp,
        "owner-without-providers",
        &[("scanner", &[OperationKind::ScanLibrary])],
    )
    .await;
    remove_node_workers(&cp, scanner_only.node_id).await;
    let file_version_id = owned_root_with_file(&cp, scanner_only.node_id).await;

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg"]),
            &one_target(file_version_id),
        )
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("no agent-supervised workers are registered"),
        "{message}"
    );
}

/// Acceptance criterion 3, stale/retired leg: dead rows bound to the active
/// incarnation are named as such instead of silently failing dispatch later.
#[tokio::test]
async fn stale_and_retired_workers_report_a_per_node_reason() {
    let (cp, _tmp) = cp().await;
    let activated = activate_owner_node(
        &cp,
        "decaying-node",
        &[
            ("ffmpeg", &[OperationKind::TranscodeVideo]),
            ("mkvtoolnix", &[OperationKind::Remux]),
        ],
    )
    .await;
    for worker in &activated.workers {
        set_worker_status(&cp, worker.worker_id, "retired").await;
    }
    let file_version_id = owned_root_with_file(&cp, activated.node_id).await;

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg", "mkvtoolnix"]),
            &one_target(file_version_id),
        )
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("every worker bound to the active incarnation is stale or retired"),
        "{message}"
    );
    assert_eq!(message.matches("- ffmpeg on node").count(), 1);
    assert_eq!(message.matches("- mkvtoolnix on node").count(), 1);
}

/// Acceptance criterion 3, failed-child leg: a node that registered but whose
/// agent never completed activation carries no active incarnation, and
/// preflight reports exactly that residue instead of guessing a host.
#[tokio::test]
async fn a_node_without_an_active_incarnation_reports_it() {
    let (cp, _tmp) = cp().await;
    let owner_id = sqlx::query(
        "INSERT INTO nodes \
         (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES ('never-activated-node', 'remote', 'active', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    let owner = NodeId(u64::try_from(owner_id).unwrap());
    let file_version_id = owned_root_with_file(&cp, owner).await;

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg"]),
            &one_target(file_version_id),
        )
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("owner node has no active agent incarnation"),
        "{message}"
    );
}

#[tokio::test]
async fn an_expired_owner_heartbeat_is_not_ready_before_the_stale_reaper_runs() {
    let (cp, _tmp) = cp().await;
    let owner = activate_owner_node(
        &cp,
        "expired-heartbeat-node",
        &[("ffmpeg", &[OperationKind::TranscodeVideo])],
    )
    .await;
    let file_version_id = owned_root_with_file(&cp, owner.node_id).await;
    sqlx::query(
        "UPDATE nodes SET last_seen_at = '1970-01-01T00:00:00Z', \
         heartbeat_ttl_seconds = 1 WHERE id = ?1",
    )
    .bind(i64::try_from(owner.node_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg"]),
            &one_target(file_version_id),
        )
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("owner node heartbeat has expired"),
        "{error}"
    );
}

/// Wrong child identity, protocol-version mismatch, and executable-preflight
/// failure all end initial agent startup through this same durable residue.
#[tokio::test]
async fn a_child_startup_failure_reports_the_owner_node_stale() {
    let (cp, _tmp) = cp().await;
    let activated = activate_owner_node(
        &cp,
        "deactivated-node",
        &[("ffmpeg", &[OperationKind::TranscodeVideo])],
    )
    .await;
    let file_version_id = owned_root_with_file(&cp, activated.node_id).await;
    // The agent-side child tests exercise each startup boundary. Once the
    // agent reports that failure, the pointer clears, the node goes stale,
    // and the incarnation ends failed.
    sqlx::query(
        "UPDATE nodes SET status = 'stale', active_incarnation_id = NULL, \
         epoch = epoch + 1 WHERE id = ?1",
    )
    .bind(i64::try_from(activated.node_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE node_incarnations SET status = 'failed', ended_at = '1970-01-01T00:00:01Z', \
         end_reason = 'child_startup_failed' WHERE incarnation_id = ?1",
    )
    .bind(activated.incarnation_id.to_string())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg"]),
            &one_target(file_version_id),
        )
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("owner node is stale"), "{message}");
}

/// Acceptance criterion 3, eligibility legs: deny wins over grant, a missing
/// grant row reads differently from a missing capability row, and each reason
/// lands on its own node's line.
#[tokio::test]
async fn denied_ungranted_and_uncapabled_workers_report_distinct_reasons() {
    let (cp, _tmp) = cp().await;
    let denied = activate_owner_node(
        &cp,
        "denied-node",
        &[("ffmpeg", &[OperationKind::TranscodeVideo])],
    )
    .await;
    cp.record_grant(NewGrant {
        worker_id: denied.workers[0].worker_id,
        can_execute: Vec::new(),
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: vec![TicketOperation::from(OperationKind::TranscodeVideo)],
        max_parallel: json!({}),
    })
    .await
    .unwrap();
    let ungranted = activate_owner_node(
        &cp,
        "ungranted-node",
        &[("ffprobe", &[OperationKind::ProbeFile])],
    )
    .await;
    delete_worker_grants(&cp, ungranted.workers[0].worker_id).await;
    let uncapabled = activate_owner_node(
        &cp,
        "uncapabled-node",
        &[("mkvtoolnix", &[OperationKind::Remux])],
    )
    .await;
    delete_worker_capabilities(&cp, uncapabled.workers[0].worker_id).await;
    let denied_file = owned_root_with_file(&cp, denied.node_id).await;
    let ungranted_file = owned_root_with_file(&cp, ungranted.node_id).await;
    let uncapabled_file = owned_root_with_file(&cp, uncapabled.node_id).await;

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg", "ffprobe", "mkvtoolnix"]),
            &[
                PolicyToolTarget {
                    ordinal: 0,
                    file_version_id: denied_file,
                },
                PolicyToolTarget {
                    ordinal: 1,
                    file_version_id: ungranted_file,
                },
                PolicyToolTarget {
                    ordinal: 2,
                    file_version_id: uncapabled_file,
                },
            ],
        )
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("on node \"denied-node\"")
            && message.contains("denied and none is effective"),
        "{message}"
    );
    assert!(
        message.contains("on node \"ungranted-node\"")
            && message.contains("capability but no execute grant"),
        "{message}"
    );
    assert!(
        message.contains("on node \"uncapabled-node\"")
            && message.contains("do not advertise a matching capability"),
        "{message}"
    );
}

/// Acceptance criterion 4: multi-root policies report every unavailable
/// (node, tool) pair in one pass, ordered by node then metadata order.
#[tokio::test]
async fn multi_node_targets_report_all_unavailable_pairs_deterministically() {
    let (cp, _tmp) = cp().await;
    let first = activate_owner_node(
        &cp,
        "empty-node-one",
        &[("scanner", &[OperationKind::ScanLibrary])],
    )
    .await;
    let second = activate_owner_node(
        &cp,
        "empty-node-two",
        &[("scanner", &[OperationKind::ScanLibrary])],
    )
    .await;
    let first_file = owned_root_with_file(&cp, first.node_id).await;
    let second_file = owned_root_with_file(&cp, second.node_id).await;

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["mkvtoolnix", "ffmpeg"]),
            &[
                PolicyToolTarget {
                    ordinal: 0,
                    file_version_id: first_file,
                },
                PolicyToolTarget {
                    ordinal: 1,
                    file_version_id: second_file,
                },
            ],
        )
        .await
        .unwrap_err();

    let message = error.to_string();
    assert_eq!(message.matches("on node \"empty-node-one\"").count(), 2);
    assert_eq!(message.matches("on node \"empty-node-two\"").count(), 2);
    let one_mkv = message
        .find("- mkvtoolnix on node \"empty-node-one\"")
        .unwrap();
    let one_ffmpeg = message.find("- ffmpeg on node \"empty-node-one\"").unwrap();
    let two_mkv = message
        .find("- mkvtoolnix on node \"empty-node-two\"")
        .unwrap();
    assert!(one_mkv < one_ffmpeg, "{message}");
    assert!(one_ffmpeg < two_mkv, "{message}");
}

/// A target with no live rooted location can never open bytes anywhere.
#[tokio::test]
async fn an_unlocated_target_is_reported_before_tool_observation() {
    let (cp, _tmp) = cp().await;

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg"]),
            &one_target(FileVersionId(424_242)),
        )
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("target 0"), "{message}");
    assert!(
        !message.contains("on node \""),
        "tools were observed despite no resolvable target: {message}"
    );
}

#[tokio::test]
async fn a_target_location_database_failure_propagates() {
    let (cp, _tmp) = cp().await;
    cp.pool_for_test().close().await;

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg"]),
            &one_target(FileVersionId(424_242)),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code(), "DB_UNREACHABLE");
}

#[tokio::test]
async fn a_policy_without_owner_scoped_requirements_does_not_resolve_targets() {
    let (cp, _tmp) = cp().await;

    cp.preflight_policy_tools(
        &mut policy_requiring(&[]),
        &one_target(FileVersionId(424_242)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn an_unavailable_storage_root_is_not_ready_even_with_a_healthy_owner() {
    let (cp, _tmp) = cp().await;
    let owner = activate_owner_node(
        &cp,
        "unavailable-root-owner",
        &[("ffmpeg", &[OperationKind::TranscodeVideo])],
    )
    .await;
    let file_version_id = owned_root_with_file(&cp, owner.node_id).await;
    let root_id: i64 =
        sqlx::query_scalar("SELECT storage_root_id FROM file_locations WHERE file_version_id = ?1")
            .bind(i64::try_from(file_version_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    sqlx::query("UPDATE library_roots SET state = 'unavailable' WHERE id = ?1")
        .bind(root_id)
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffmpeg"]),
            &one_target(file_version_id),
        )
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("storage root is unavailable"),
        "{error}"
    );
}
#[tokio::test]
async fn unavailable_requirements_are_aggregated_in_metadata_order() {
    let (cp, _tmp) = cp().await;
    let owner = activate_owner_node(
        &cp,
        "aggregating-node",
        &[("scanner", &[OperationKind::ScanLibrary])],
    )
    .await;
    let file_version_id = owned_root_with_file(&cp, owner.node_id).await;
    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["mkvtoolnix", "ffmpeg"]),
            &one_target(file_version_id),
        )
        .await
        .unwrap_err()
        .to_string();

    let mkvtoolnix = error.find("- mkvtoolnix on node").unwrap();
    let ffmpeg = error.find("- ffmpeg on node").unwrap();
    assert!(mkvtoolnix < ffmpeg, "unexpected diagnostic order: {error}");
}

#[tokio::test]
async fn malformed_stored_requirements_fail_before_provider_observation() {
    let (cp, _tmp) = cp().await;
    let mut policy = policy_requiring(&[]);
    policy
        .metadata
        .insert("requires_tools".to_owned(), json!("ffmpeg"));

    let error = cp
        .preflight_policy_tools(&mut policy, &[])
        .await
        .unwrap_err();

    assert_eq!(error.code(), "POLICY_EXECUTION_ERROR");
    assert!(
        error
            .to_string()
            .contains("metadata.requires_tools must be an array")
    );
}

#[test]
fn typing_stored_requirements_removes_only_the_obsolete_warning() {
    let mut policy = policy_requiring(&["ffmpeg"]);
    let warning = |code: &str| PolicyDiagnostic {
        code: code.to_owned(),
        severity: DiagnosticSeverity::Warning,
        stage: DiagnosticStage::Validate,
        span: SourceSpan::new(0, 1),
        location: SourceLocation { line: 1, column: 1 },
        message: code.to_owned(),
        suggestion: None,
        related: Vec::new(),
    };
    policy
        .warnings
        .push(warning("metadata_requires_tools_deferred"));
    policy.warnings.push(warning("another_warning"));

    let tools = super::normalize_policy_tool_requirements(&mut policy).unwrap();

    assert_eq!(tools, vec![PolicyTool::Ffmpeg]);
    assert_eq!(policy.warnings.len(), 1);
    assert_eq!(policy.warnings[0].code, "another_warning");
}

#[tokio::test]
async fn gpu_bound_worker_does_not_satisfy_software_profile_preflight() {
    let (cp, _tmp) = cp().await;
    let worker = cp
        .register_supervisor_worker(NewWorker {
            name: "local-ffmpeg-gpu".to_owned(),
            kind: WorkerKind::Local,
            registered_at: cp.clock().now(),
            node_id: None,
        })
        .await
        .unwrap();
    let operation = TicketOperation::from(OperationKind::TranscodeVideo);
    let hardware_token = "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: vec![hardware_token.to_owned()],
        artifact_access: Vec::new(),
        extra: json!({
            "accelerator": {
                "backend": "nvidia",
                "hardware_token": hardware_token,
                "device_uuid": "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "device_name": "Test GPU",
                "driver_version": "595.80",
                "encoders": ["hevc_nvenc"],
                "decoders": ["h264_cuvid"],
                "max_sessions": 2
            }
        }),
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: json!({"transcode_video": 2}),
    })
    .await
    .unwrap();
    let policy = compile_policy(
        "policy \"software\" { \
         metadata { requires_tools: [ffmpeg] } \
         phase encode { transcode video to hevc { \
         encoder: libx265 crf: 23 preset: medium } } }",
    )
    .unwrap()
    .policy;

    let error = preflight_video_hardware_for_test(&cp, &policy, &[&worker])
        .await
        .unwrap_err();

    assert_eq!(error.code(), "POLICY_EXECUTION_ERROR");
    assert!(
        error
            .to_string()
            .contains("software transcode profiles require an unbound ffmpeg worker")
    );
}

/// A VAAPI profile needs a live device that probed `hevc_vaapi`.
/// A software worker cannot substitute — that is the fallback issue #409 forbids —
/// and neither can a VAAPI device whose driver build never proved the encoder, which
/// on the acceptance host is what stock `mesa-dri-drivers` looks like (ADR 0052 §2).
#[tokio::test]
async fn a_vaapi_transcode_requires_a_proven_hevc_vaapi_descriptor() {
    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::from(OperationKind::TranscodeVideo);
    let software = register_transcode_worker(
        &cp,
        "local-ffmpeg-software",
        &operation,
        Vec::new(),
        json!({}),
        1,
    )
    .await;
    // Proven for AV1 only, exactly what the stock Mesa driver build advertises.
    let av1_only = register_transcode_worker(
        &cp,
        "local-ffmpeg-vaapi",
        &operation,
        vec!["vaapi:pci-0000:f4:00.0".to_owned()],
        vaapi_accelerator_extra(&["av1_vaapi"], &["hevc"]),
        2,
    )
    .await;
    let policy = vaapi_policy("");

    let error = preflight_video_hardware_for_test(&cp, &policy, &[&software, &av1_only])
        .await
        .unwrap_err();

    assert_eq!(error.code(), "POLICY_EXECUTION_ERROR");
    assert!(
        error
            .to_string()
            .contains("hevc_vaapi profiles require a live VAAPI-bound ffmpeg worker"),
        "{error}"
    );

    let proven = register_transcode_worker(
        &cp,
        "local-ffmpeg-vaapi-hevc",
        &operation,
        vec!["vaapi:pci-0000:f4:00.0".to_owned()],
        vaapi_accelerator_extra(&["hevc_vaapi"], &["hevc"]),
        2,
    )
    .await;
    preflight_video_hardware_for_test(&cp, &policy, &[&software, &proven])
        .await
        .unwrap();
}

/// A `vaapi`-decode profile needs the device to have probed a decoder as well as the
/// encoder. Exact source-codec compatibility stays per-file; preflight only refuses a
/// device that proved no decoder at all.
#[tokio::test]
async fn a_vaapi_decode_policy_requires_at_least_one_probed_vaapi_decoder() {
    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::from(OperationKind::TranscodeVideo);
    let no_decoders = register_transcode_worker(
        &cp,
        "local-ffmpeg-vaapi",
        &operation,
        vec!["vaapi:pci-0000:f4:00.0".to_owned()],
        vaapi_accelerator_extra(&["hevc_vaapi"], &[]),
        2,
    )
    .await;
    let policy = vaapi_policy(" decode: vaapi");

    let error = preflight_video_hardware_for_test(&cp, &policy, &[&no_decoders])
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("with at least one probed VAAPI decoder"),
        "{error}"
    );

    // The same policy passes once a device has proven a decoder, so the gate is the
    // decoder list and not the decode clause itself.
    let with_decoders = register_transcode_worker(
        &cp,
        "local-ffmpeg-vaapi-decode",
        &operation,
        vec!["vaapi:pci-0000:f4:00.0".to_owned()],
        vaapi_accelerator_extra(&["hevc_vaapi"], &["h264", "hevc", "av1"]),
        2,
    )
    .await;
    preflight_video_hardware_for_test(&cp, &policy, &[&with_decoders])
        .await
        .unwrap();
}

/// ADR 0049 §6 again, for the case a rolling upgrade actually produces: a worker
/// advertising a backend tag this build has never heard of. Parsing it is an error,
/// and letting that error escape would turn one unknown worker into a job-fatal
/// failure for every policy on the fleet. It must instead contribute no
/// availability, so a healthy sibling still satisfies the policy.
#[tokio::test]
async fn an_unreadable_descriptor_on_one_worker_does_not_fail_the_whole_fleet() {
    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::from(OperationKind::TranscodeVideo);
    let from_the_future = register_transcode_worker(
        &cp,
        "local-ffmpeg-unknown",
        &operation,
        vec!["qsv:pci-0000:00:02.0".to_owned()],
        json!({ "accelerator": { "backend": "qsv", "device": "/dev/dri/renderD128" } }),
        2,
    )
    .await;
    let policy = vaapi_policy("");

    // Alone, the unreadable worker leaves VAAPI unavailable — a clear preflight
    // message, not a repository error.
    let error = preflight_video_hardware_for_test(&cp, &policy, &[&from_the_future])
        .await
        .unwrap_err();
    assert!(
        matches!(error, VoomError::PolicyExecution(_)),
        "expected a preflight failure, got: {error}"
    );

    // Beside a healthy VAAPI worker, the policy passes: the unknown worker did not
    // poison the projection.
    let healthy = register_transcode_worker(
        &cp,
        "local-ffmpeg-vaapi",
        &operation,
        vec!["vaapi:pci-0000:f4:00.0".to_owned()],
        vaapi_accelerator_extra(&["hevc_vaapi"], &["h264", "hevc"]),
        2,
    )
    .await;
    preflight_video_hardware_for_test(&cp, &policy, &[&from_the_future, &healthy])
        .await
        .unwrap();
}

/// A host may run a software worker beside a VAAPI-bound one. ADR 0049 §6 forbids
/// an error escaping candidate projection, so the VAAPI worker's descriptor must
/// not decide whether a software profile can be scheduled: preflight has to keep
/// observing the software worker and pass.
#[tokio::test]
async fn a_live_vaapi_worker_does_not_break_software_profile_preflight() {
    let (cp, _tmp) = cp().await;
    let operation = TicketOperation::from(OperationKind::TranscodeVideo);
    let software = register_transcode_worker(
        &cp,
        "local-ffmpeg-software",
        &operation,
        Vec::new(),
        json!({}),
        1,
    )
    .await;
    let vaapi = register_transcode_worker(
        &cp,
        "local-ffmpeg-vaapi",
        &operation,
        vec!["vaapi:pci-0000:f4:00.0".to_owned()],
        vaapi_accelerator_extra(&["hevc_vaapi"], &["hevc"]),
        2,
    )
    .await;
    let policy = compile_policy(
        "policy \"software\" { \
         metadata { requires_tools: [ffmpeg] } \
         phase encode { transcode video to hevc { \
         encoder: libx265 crf: 23 preset: medium } } }",
    )
    .unwrap()
    .policy;

    preflight_video_hardware_for_test(&cp, &policy, &[&software, &vaapi])
        .await
        .unwrap();
}

#[tokio::test]
async fn nvidia_decode_profile_requires_an_advertised_cuvid_decoder() {
    let (cp, _tmp) = cp().await;
    let worker = cp
        .register_supervisor_worker(NewWorker {
            name: "local-ffmpeg-gpu".to_owned(),
            kind: WorkerKind::Local,
            registered_at: cp.clock().now(),
            node_id: None,
        })
        .await
        .unwrap();
    let operation = TicketOperation::from(OperationKind::TranscodeVideo);
    let hardware_token = "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: vec![hardware_token.to_owned()],
        artifact_access: Vec::new(),
        extra: json!({
            "accelerator": {
                "backend": "nvidia",
                "hardware_token": hardware_token,
                "device_uuid": "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "device_name": "Test GPU",
                "driver_version": "595.80",
                "encoders": ["hevc_nvenc"],
                "decoders": [],
                "max_sessions": 2
            }
        }),
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: json!({"transcode_video": 2}),
    })
    .await
    .unwrap();
    let policy = compile_policy(
        "policy \"gpu-decode\" { \
         metadata { requires_tools: [ffmpeg] } \
         phase encode { transcode video to hevc { \
         encoder: hevc_nvenc cq: 23 preset: p4 decode: nvidia } } }",
    )
    .unwrap()
    .policy;

    let error = preflight_video_hardware_for_test(&cp, &policy, &[&worker])
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("with at least one advertised CUVID decoder")
    );
}

fn vaapi_policy(extra_settings: &str) -> CompiledPolicy {
    compile_policy(&format!(
        "policy \"vaapi\" {{ \
         metadata {{ requires_tools: [ffmpeg] }} \
         phase encode {{ transcode video to hevc {{ \
         encoder: hevc_vaapi qp: 24{extra_settings} }} }} }}"
    ))
    .unwrap()
    .policy
}

/// Registers one live `transcode_video` provider, so a test can describe a
/// whole host by listing workers.
async fn register_transcode_worker(
    cp: &crate::ControlPlane,
    name: &str,
    operation: &TicketOperation,
    hardware: Vec<String>,
    extra: serde_json::Value,
    max_parallel: u32,
) -> voom_store::repo::execution::workers::Worker {
    let worker = cp
        .register_supervisor_worker(NewWorker {
            name: name.to_owned(),
            kind: WorkerKind::Local,
            registered_at: cp.clock().now(),
            node_id: None,
        })
        .await
        .unwrap();
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware,
        artifact_access: Vec::new(),
        extra,
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation.clone()],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: json!({operation.as_str(): max_parallel}),
    })
    .await
    .unwrap();
    worker
}

async fn preflight_video_hardware_for_test(
    cp: &crate::ControlPlane,
    policy: &CompiledPolicy,
    workers: &[&voom_store::repo::execution::workers::Worker],
) -> Result<(), VoomError> {
    let live_worker_ids = workers.iter().map(|worker| worker.id).collect();
    let owners = std::collections::BTreeMap::from([(
        NodeId(1),
        super::OwnerWorkerSet {
            node_name: "hardware-test-owner".to_owned(),
            live_worker_ids,
        },
    )]);
    let requirements = super::policy_video_backend_requirements(policy)?;
    cp.preflight_video_hardware(&policy.slug, &requirements, &owners)
        .await
}

/// The `backend`-tagged extras a VAAPI-bound worker stores, per ADR 0052 §1: the
/// device is named by PCI address and carries no `hardware_token` field.
fn vaapi_accelerator_extra(encoders: &[&str], decoders: &[&str]) -> serde_json::Value {
    json!({
        "accelerator": {
            "backend": "vaapi",
            "pci_address": "0000:f4:00.0",
            "device_name": "AMD Radeon 8060S Graphics",
            "driver_version": "Mesa Gallium 26.1.5 radeonsi",
            "encoders": encoders,
            "decoders": decoders,
            "max_sessions": 2
        }
    })
}

struct ActivatedOwner {
    node_id: NodeId,
    incarnation_id: voom_core::NodeIncarnationId,
    workers: Vec<crate::cases::execution::remote_execution::ActivatedWorker>,
}

/// Register and activate a remote node through the production case functions,
/// declaring one supervised worker per `(logical_name, operations)` pair — the
/// same durable shape a real node agent's manifest produces.
async fn activate_owner_node(
    cp: &crate::ControlPlane,
    slug: &str,
    workers: &[(&str, &[OperationKind])],
) -> ActivatedOwner {
    let registered = cp
        .register_node(RegisterNodeInput {
            name: slug.to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let declarations = workers
        .iter()
        .map(|(logical_name, operations)| RemoteWorkerDeclaration {
            logical_name: (*logical_name).to_owned(),
            operations: (*operations).to_vec(),
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            max_parallel: 1,
        })
        .collect();
    let call = next_seq();
    let incarnation_id: voom_core::NodeIncarnationId = format!("{call:032x}").parse().unwrap();
    let outcome = cp
        .remote_activate(RemoteActivateInput {
            node_id: registered.node.id,
            token: registered.token,
            idempotency_key: format!("activate-{slug}-{call}"),
            request_hash: format!("activation-body-{slug}-{call}"),
            incarnation_id,
            workers: declarations,
        })
        .await
        .unwrap();
    ActivatedOwner {
        node_id: registered.node.id,
        incarnation_id: outcome.incarnation_id,
        workers: outcome.workers,
    }
}

/// Create a library, an active root owned by `owner`, and one ingested file
/// version with a live rooted location on that root.
async fn owned_root_with_file(cp: &crate::ControlPlane, owner: NodeId) -> FileVersionId {
    let call = next_seq();
    let slug = format!("tool-owner-{call}");
    let lib = cp
        .create_library(NewLibrary {
            slug: slug.clone(),
            display_name: slug.clone(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = cp
        .create_library_root(NewLibraryRoot {
            library_id: lib.id,
            owner_node_id: owner,
            provider_kind: voom_core::StorageProviderKind::LocalFilesystem,
            provider_locator: voom_core::ProviderLocator::new(format!("/media/{slug}")).unwrap(),
            display_locator: format!("/media/{slug}"),
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extension_allowlist: Vec::new(),
            scan_mode: LibraryScanMode::ManualRecursive,
            symlink_policy: SymlinkPolicy::Reject,
            hidden_file_policy: HiddenFilePolicy::Ignore,
            max_depth: None,
            stability_seconds: 0,
            debounce_seconds: 0,
            default_output_root_id: None,
            default_staging_root_id: None,
            default_backup_root_id: None,
            enabled: true,
        })
        .await
        .unwrap();
    cp.activate_library_root(root.id, format!("test:{call}"))
        .await
        .unwrap();
    match cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: root.id,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    "movie.mp4",
                ),
                content_hash: format!("hash-{call}"),
                size_bytes: 1024,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap()
    {
        IngestOutcome::NewFileAsset {
            file_version_id, ..
        } => file_version_id,
        other @ IngestOutcome::AliasAttached { .. } => {
            panic!("expected new file asset, got {other:?}")
        }
    }
}

fn one_target(file_version_id: FileVersionId) -> Vec<PolicyToolTarget> {
    vec![PolicyToolTarget {
        ordinal: 0,
        file_version_id,
    }]
}

async fn set_worker_status(cp: &crate::ControlPlane, worker_id: WorkerId, status: &str) {
    let retired_at = if status == "retired" {
        "'1970-01-01T00:00:00Z'"
    } else {
        "NULL"
    };
    sqlx::query(&format!(
        "UPDATE workers SET status = ?2, retired_at = {retired_at} WHERE id = ?1"
    ))
    .bind(i64::try_from(worker_id.0).unwrap())
    .bind(status)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

/// Remove every durable worker of one node so readiness observes a live
/// incarnation with zero registered workers.
async fn remove_node_workers(cp: &crate::ControlPlane, node_id: NodeId) {
    sqlx::query(
        "DELETE FROM worker_grants WHERE worker_id IN \
         (SELECT id FROM workers WHERE node_id = ?1)",
    )
    .bind(i64::try_from(node_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query(
        "DELETE FROM worker_capabilities WHERE worker_id IN \
         (SELECT id FROM workers WHERE node_id = ?1)",
    )
    .bind(i64::try_from(node_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query("DELETE FROM workers WHERE node_id = ?1")
        .bind(i64::try_from(node_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
}

async fn delete_worker_grants(cp: &crate::ControlPlane, worker_id: WorkerId) {
    sqlx::query("DELETE FROM worker_grants WHERE worker_id = ?")
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
}

async fn delete_worker_capabilities(cp: &crate::ControlPlane, worker_id: WorkerId) {
    sqlx::query("DELETE FROM worker_capabilities WHERE worker_id = ?")
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
}

fn policy_requiring(tools: &[&str]) -> CompiledPolicy {
    let tools = tools.join(", ");
    compile_policy(&format!(
        "policy \"published\" {{ metadata {{ requires_tools: [{tools}] }} phase inspect {{}} }}"
    ))
    .unwrap()
    .policy
}
