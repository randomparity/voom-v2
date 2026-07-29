use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::json;
use voom_core::{OperationKind, PROTOCOL_VERSION, TicketOperation, WorkerId};
use voom_policy::{
    CompiledPolicy, DiagnosticSeverity, DiagnosticStage, PolicyDiagnostic, PolicyTool,
    SourceLocation, SourceSpan, compile_policy,
};
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, OperationRequest, ProtocolError,
    WorkerCredentials, WorkerIdentityResponse,
};

use super::{UnavailableTool, format_unavailable_tools};
use crate::cases::cp;
use crate::workflow::WorkerRuntimeRegistry;

#[test]
fn unavailable_tools_are_reported_in_observation_order_with_guidance() {
    let message = format_unavailable_tools(
        "published",
        &[
            UnavailableTool {
                tool: PolicyTool::Mkvtoolnix,
                reason: "denied".to_owned(),
            },
            UnavailableTool {
                tool: PolicyTool::Ffmpeg,
                reason: "stale".to_owned(),
            },
        ],
    );

    assert_eq!(
        message,
        "tool requirement preflight failed for policy `published`:\n\
         - mkvtoolnix: denied; start one with: voom worker run-local --kind mkvtoolnix\n\
         - ffmpeg: stale; start one with: voom worker run-local --kind ffmpeg"
    );
}

#[tokio::test]
async fn live_reserved_provider_requires_effective_grant_and_matching_identity() {
    let (cp, _tmp) = cp().await;
    let worker = cp
        .register_supervisor_worker(NewWorker {
            name: "local-ffmpeg-test".to_owned(),
            kind: WorkerKind::Local,
            registered_at: cp.clock().now(),
            node_id: None,
        })
        .await
        .unwrap();
    let operation = TicketOperation::from(OperationKind::TranscodeVideo);
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra: json!({}),
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation.clone()],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: json!({}),
    })
    .await
    .unwrap();
    let worker_credentials = credentials(worker.id, worker.epoch);
    let registry = WorkerRuntimeRegistry::new().with_in_process_runtime(
        worker.id,
        Arc::new(IdentityClient {
            worker_id: worker.id,
            worker_epoch: worker.epoch,
            handshake_ok: true,
        }),
        worker_credentials,
    );
    let mut policy = policy_requiring(&["ffmpeg"]);

    cp.preflight_policy_tools(&mut policy, &registry)
        .await
        .unwrap();

    let dead_registry = WorkerRuntimeRegistry::new().with_in_process_runtime(
        worker.id,
        Arc::new(IdentityClient {
            worker_id: worker.id,
            worker_epoch: worker.epoch,
            handshake_ok: false,
        }),
        credentials(worker.id, worker.epoch),
    );
    let error = cp
        .preflight_policy_tools(&mut policy, &dead_registry)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("handshake failed"));

    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: Vec::new(),
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: vec![operation],
        max_parallel: json!({}),
    })
    .await
    .unwrap();
    let error = cp
        .preflight_policy_tools(&mut policy, &registry)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("is denied and none is effective")
    );
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
    let registry = WorkerRuntimeRegistry::new().with_in_process_runtime(
        worker.id,
        Arc::new(IdentityClient {
            worker_id: worker.id,
            worker_epoch: worker.epoch,
            handshake_ok: true,
        }),
        credentials(worker.id, worker.epoch),
    );
    let mut policy = compile_policy(
        "policy \"software\" { \
         metadata { requires_tools: [ffmpeg] } \
         phase encode { transcode video to hevc { \
         encoder: libx265 crf: 23 preset: medium } } }",
    )
    .unwrap()
    .policy;

    let error = cp
        .preflight_policy_tools(&mut policy, &registry)
        .await
        .unwrap_err();

    assert_eq!(error.code(), "POLICY_EXECUTION_ERROR");
    assert!(
        error
            .to_string()
            .contains("software transcode profiles require an unbound ffmpeg worker")
    );
}

#[tokio::test]
async fn endpoint_tool_rejects_runtime_identity_for_another_worker() {
    let (cp, _tmp) = cp().await;
    let worker = cp
        .register_supervisor_worker(NewWorker {
            name: "local-mkvtoolnix-test".to_owned(),
            kind: WorkerKind::Local,
            registered_at: cp.clock().now(),
            node_id: None,
        })
        .await
        .unwrap();
    let operation = TicketOperation::from(OperationKind::Remux);
    cp.record_capability(NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra: json!({}),
    })
    .await
    .unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: json!({}),
    })
    .await
    .unwrap();
    let registry = WorkerRuntimeRegistry::new().with_in_process_runtime(
        worker.id,
        Arc::new(IdentityClient {
            worker_id: WorkerId(worker.id.0 + 1),
            worker_epoch: worker.epoch,
            handshake_ok: true,
        }),
        credentials(worker.id, worker.epoch),
    );

    let error = cp
        .preflight_policy_tools(&mut policy_requiring(&["mkvtoolnix"]), &registry)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("returned identity"));
}

#[tokio::test]
async fn unavailable_requirements_are_aggregated_in_metadata_order() {
    let (cp, _tmp) = cp().await;
    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["mkvtoolnix", "ffmpeg"]),
            &WorkerRuntimeRegistry::new(),
        )
        .await
        .unwrap_err()
        .to_string();

    let mkvtoolnix = error.find("- mkvtoolnix:").unwrap();
    let ffmpeg = error.find("- ffmpeg:").unwrap();
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
        .preflight_policy_tools(&mut policy, &WorkerRuntimeRegistry::new())
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
async fn denied_ffprobe_is_aggregated_with_later_missing_tool() {
    let (cp, _tmp) = cp().await;
    let mut tx = crate::cases::begin_immediate_tx(cp.pool_for_test())
        .await
        .unwrap();
    let worker = crate::scan::bootstrap::ensure_builtin_ffprobe_worker_in_tx(&cp, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    cp.record_grant(NewGrant {
        worker_id: worker.id,
        can_execute: Vec::new(),
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: vec![TicketOperation::from(OperationKind::ProbeFile)],
        max_parallel: json!({}),
    })
    .await
    .unwrap();

    let error = cp
        .preflight_policy_tools(
            &mut policy_requiring(&["ffprobe", "ffmpeg"]),
            &WorkerRuntimeRegistry::new(),
        )
        .await
        .unwrap_err();
    let message = error.to_string();

    assert_eq!(error.code(), "POLICY_EXECUTION_ERROR");
    assert!(message.contains("- ffprobe: live built-in provider"));
    assert!(message.contains("denied probe_file"));
    assert!(message.find("- ffprobe:").unwrap() < message.find("- ffmpeg:").unwrap());
}

fn policy_requiring(tools: &[&str]) -> CompiledPolicy {
    let tools = tools.join(", ");
    compile_policy(&format!(
        "policy \"published\" {{ metadata {{ requires_tools: [{tools}] }} phase inspect {{}} }}"
    ))
    .unwrap()
    .policy
}

fn credentials(worker_id: WorkerId, worker_epoch: u64) -> WorkerCredentials {
    WorkerCredentials {
        worker_id,
        worker_epoch,
        secret: SecretString::from("test-secret"),
    }
}

#[derive(Debug)]
struct IdentityClient {
    worker_id: WorkerId,
    worker_epoch: u64,
    handshake_ok: bool,
}

#[async_trait]
impl ClientHandle for IdentityClient {
    async fn handshake(&self, _offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        if !self.handshake_ok {
            return Err(ProtocolError::InternalServerError);
        }
        Ok(HandshakeResponse {
            agreed: PROTOCOL_VERSION,
        })
    }

    async fn identity(
        &self,
        _credentials: &WorkerCredentials,
    ) -> Result<WorkerIdentityResponse, ProtocolError> {
        Ok(WorkerIdentityResponse {
            worker_id: self.worker_id,
            worker_epoch: self.worker_epoch,
            protocol_version: PROTOCOL_VERSION,
            proof: "verified-by-client-boundary".to_owned(),
        })
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        _request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }
}
