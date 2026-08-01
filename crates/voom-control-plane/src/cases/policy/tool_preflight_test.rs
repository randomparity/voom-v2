use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::json;
use voom_core::{OperationKind, PROTOCOL_VERSION, TicketOperation, VoomError, WorkerId};
use voom_policy::{
    CompiledPolicy, DiagnosticSeverity, DiagnosticStage, PolicyDiagnostic, PolicyTool,
    SourceLocation, SourceSpan, compile_policy,
};
use voom_store::repo::execution::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, OperationRequest, ProtocolError,
    WorkerCredentials, WorkerIdentityResponse,
};

use super::{UnavailableTool, format_unavailable_tools, policy_video_backend_requirements};
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

    let requirements = policy_video_backend_requirements(&policy).unwrap();

    assert!(!requirements.software);
    assert!(!requirements.nvidia.required);
    assert!(requirements.videotoolbox.hardware_decode);
    assert!(
        requirements
            .videotoolbox_encoders
            .contains("hevc_videotoolbox")
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

/// A VAAPI profile needs a live, identity-verified device that probed `hevc_vaapi`.
/// A software worker cannot substitute — that is the fallback issue #409 forbids —
/// and neither can a VAAPI device whose driver build never proved the encoder, which
/// on the acceptance host is what stock `mesa-dri-drivers` looks like (ADR 0052 §2).
#[tokio::test]
async fn a_vaapi_transcode_requires_an_identity_verified_hevc_vaapi_descriptor() {
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
    let mut policy = vaapi_policy("");

    let error = cp
        .preflight_policy_tools(&mut policy, &live_registry(&[&software, &av1_only]))
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
    cp.preflight_policy_tools(&mut policy, &live_registry(&[&software, &proven]))
        .await
        .unwrap();

    // The very same durable descriptor, with no live endpoint to verify identity
    // against, must not satisfy preflight.
    let error = cp
        .preflight_policy_tools(&mut policy, &live_registry(&[&software]))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("hevc_vaapi profiles require a live VAAPI-bound ffmpeg worker"),
        "{error}"
    );
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
    let mut policy = vaapi_policy(" decode: vaapi");

    let error = cp
        .preflight_policy_tools(&mut policy, &live_registry(&[&no_decoders]))
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
    cp.preflight_policy_tools(&mut policy, &live_registry(&[&with_decoders]))
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
    let mut policy = vaapi_policy("");

    // Alone, the unreadable worker leaves VAAPI unavailable — a clear preflight
    // message, not a repository error.
    let error = cp
        .preflight_policy_tools(&mut policy, &live_registry(&[&from_the_future]))
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
    cp.preflight_policy_tools(&mut policy, &live_registry(&[&from_the_future, &healthy]))
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
    let registry = live_registry(&[&software, &vaapi]);
    let mut policy = compile_policy(
        "policy \"software\" { \
         metadata { requires_tools: [ffmpeg] } \
         phase encode { transcode video to hevc { \
         encoder: libx265 crf: 23 preset: medium } } }",
    )
    .unwrap()
    .policy;

    cp.preflight_policy_tools(&mut policy, &registry)
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
        "policy \"gpu-decode\" { \
         metadata { requires_tools: [ffmpeg] } \
         phase encode { transcode video to hevc { \
         encoder: hevc_nvenc cq: 23 preset: p4 decode: nvidia } } }",
    )
    .unwrap()
    .policy;

    let error = cp
        .preflight_policy_tools(&mut policy, &registry)
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("with at least one advertised CUVID decoder")
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

/// Registers one live `transcode_video` provider with the reserved local naming
/// preflight looks for, so a test can describe a whole host by listing workers.
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

fn live_registry(
    workers: &[&voom_store::repo::execution::workers::Worker],
) -> WorkerRuntimeRegistry {
    let mut registry = WorkerRuntimeRegistry::new();
    for worker in workers {
        registry = registry.with_in_process_runtime(
            worker.id,
            Arc::new(IdentityClient {
                worker_id: worker.id,
                worker_epoch: worker.epoch,
                handshake_ok: true,
            }),
            credentials(worker.id, worker.epoch),
        );
    }
    registry
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
