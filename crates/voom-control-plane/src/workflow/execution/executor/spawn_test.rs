use super::*;

fn software_candidate(worker_id: u64) -> WorkerOperationCandidate {
    WorkerOperationCandidate {
        worker_id: WorkerId(worker_id),
        active_leases: 0,
        max_parallel: 1,
        hardware: Vec::new(),
        capability_extra: vec![serde_json::json!({"endpoint": "http://software"})],
    }
}

fn nvidia_candidate(
    worker_id: u64,
    token: &str,
    max_sessions: u32,
    decoders: &[&str],
) -> WorkerOperationCandidate {
    WorkerOperationCandidate {
        worker_id: WorkerId(worker_id),
        active_leases: 0,
        max_parallel: max_sessions,
        hardware: vec![token.to_owned()],
        capability_extra: vec![serde_json::json!({
            "accelerator": {
                "backend": "nvidia",
                "hardware_token": token,
                "device_uuid": token.trim_start_matches("nvidia:"),
                "device_name": "Test GPU",
                "driver_version": "595.80",
                "encoders": ["hevc_nvenc"],
                "decoders": decoders,
                "max_sessions": max_sessions
            }
        })],
    }
}

fn vaapi_candidate(
    worker_id: u64,
    pci_address: &str,
    max_sessions: u32,
    encoders: &[&str],
    decoders: &[&str],
) -> WorkerOperationCandidate {
    WorkerOperationCandidate {
        worker_id: WorkerId(worker_id),
        active_leases: 0,
        max_parallel: max_sessions,
        hardware: vec![format!("vaapi:pci-{pci_address}")],
        capability_extra: vec![serde_json::json!({
            "accelerator": {
                "backend": "vaapi",
                "pci_address": pci_address,
                "device_name": "AMD Radeon 8060S Graphics",
                "driver_version": "Mesa Gallium 26.1.5 radeonsi",
                "encoders": encoders,
                "decoders": decoders,
                "max_sessions": max_sessions
            }
        })],
    }
}

fn videotoolbox_candidate(
    worker_id: u64,
    token: &str,
    max_sessions: u32,
    encoders: &[&str],
    decoders: &serde_json::Value,
) -> WorkerOperationCandidate {
    WorkerOperationCandidate {
        worker_id: WorkerId(worker_id),
        active_leases: 0,
        max_parallel: max_sessions,
        hardware: vec![token.to_owned()],
        capability_extra: vec![serde_json::json!({
            "accelerator": {
                "backend": "video_toolbox",
                "hardware_token": token,
                "resource_id": token.trim_start_matches("videotoolbox:"),
                "model_identifier": "Mac17,6",
                "chip_name": "Apple M5 Max",
                "macos_version": "26.5.2",
                "macos_build": "25F84",
                "encoders": encoders,
                "decoders": decoders,
                "max_sessions": max_sessions
            }
        })],
    }
}

/// ADR 0049 §6 forbids an error escaping candidate projection: one worker's
/// descriptor must never decide whether some other backend's job can be scheduled
/// at all. A live VAAPI worker beside a software or NVIDIA worker is an ordinary
/// mixed host, so every backend must still project candidates on it — and the
/// VAAPI worker must still be refused a software profile (ADR 0049 §5) rather than
/// being made invisible.
#[test]
fn a_live_vaapi_worker_does_not_poison_projection_for_other_backends() {
    let candidates = vec![
        software_candidate(1),
        nvidia_candidate(2, "nvidia:GPU-a", 2, &["hevc_cuvid"]),
        vaapi_candidate(3, "0000:f4:00.0", 2, &["hevc_vaapi"], &["hevc"]),
    ];
    let conflicts = HashSet::new();

    assert!(
        conflicting_accelerator_tokens(&candidates).is_empty(),
        "three distinct devices declare no conflicting capacity"
    );

    let software = VideoHardwareRequirement::software();
    assert_eq!(
        compatible_assignment(&candidates[0], Some(&software), &conflicts),
        CandidateCompatibility::Compatible(None)
    );
    assert_eq!(
        compatible_assignment(&candidates[2], Some(&software), &conflicts),
        CandidateCompatibility::Incompatible,
        "a device-bound VAAPI worker must not satisfy a software profile"
    );

    let nvidia = VideoHardwareRequirement::nvidia("hevc_nvenc", Some("hevc_cuvid".to_owned()));
    assert_eq!(
        compatible_assignment(&candidates[1], Some(&nvidia), &conflicts),
        CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::nvidia(
            "nvidia:GPU-a",
            "GPU-a"
        )))
    );
    assert_eq!(
        compatible_assignment(&candidates[2], Some(&nvidia), &conflicts),
        CandidateCompatibility::Incompatible,
        "a VAAPI device cannot satisfy an NVENC requirement"
    );
}

#[test]
fn software_requirement_excludes_device_bound_workers() {
    let requirement = VideoHardwareRequirement::software();
    let conflicts = HashSet::new();

    assert_eq!(
        compatible_assignment(&software_candidate(1), Some(&requirement), &conflicts),
        CandidateCompatibility::Compatible(None)
    );
    assert_eq!(
        compatible_assignment(
            &nvidia_candidate(2, "nvidia:GPU-a", 2, &["h264_cuvid"]),
            Some(&requirement),
            &conflicts,
        ),
        CandidateCompatibility::Incompatible
    );
}

#[test]
fn accelerator_runtime_loading_covers_nvidia_and_videotoolbox() {
    let software = VideoHardwareRequirement::software();
    let nvidia = VideoHardwareRequirement::nvidia("hevc_nvenc", None);
    let videotoolbox = VideoHardwareRequirement::video_toolbox("hevc_videotoolbox", None);

    assert!(!requires_accelerator(None));
    assert!(!requires_accelerator(Some(&software)));
    assert!(requires_accelerator(Some(&nvidia)));
    assert!(requires_accelerator(Some(&videotoolbox)));
}

#[test]
fn nvidia_requirement_requires_exact_encoder_and_decoder() {
    let requirement = VideoHardwareRequirement::nvidia("hevc_nvenc", Some("av1_cuvid".to_owned()));
    let conflicts = HashSet::new();
    let without_av1 = nvidia_candidate(1, "nvidia:GPU-a", 2, &["h264_cuvid", "hevc_cuvid"]);
    let with_av1 = nvidia_candidate(
        2,
        "nvidia:GPU-b",
        2,
        &["h264_cuvid", "hevc_cuvid", "av1_cuvid"],
    );

    assert_eq!(
        compatible_assignment(&without_av1, Some(&requirement), &conflicts),
        CandidateCompatibility::Incompatible
    );
    assert_eq!(
        compatible_assignment(&with_av1, Some(&requirement), &conflicts),
        CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::nvidia(
            "nvidia:GPU-b",
            "GPU-b"
        )))
    );
}

/// Replaces Task 5's fail-loud placeholder, which errored on any VAAPI
/// requirement because device selection did not exist yet.
///
/// A VAAPI requirement matches only a live, identity-verified VAAPI device: the
/// token derived from the descriptor's PCI address must still be advertised in the
/// candidate's `hardware`, and the assignment must name that same address so the
/// worker can refuse work aimed at another device (ADR 0052 §1). A software or
/// NVIDIA worker is incompatible, never an error.
#[test]
fn vaapi_requirement_matches_only_a_verified_same_device_descriptor() {
    let requirement = VideoHardwareRequirement::vaapi("hevc_vaapi", None);
    let conflicts = HashSet::new();
    let bound = vaapi_candidate(1, "0000:f4:00.0", 2, &["hevc_vaapi"], &["hevc"]);

    assert_eq!(
        compatible_assignment(&bound, Some(&requirement), &conflicts),
        CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::vaapi(
            "vaapi:pci-0000:f4:00.0",
            "0000:f4:00.0"
        )))
    );
    assert_eq!(
        compatible_assignment(&software_candidate(2), Some(&requirement), &conflicts),
        CandidateCompatibility::Incompatible
    );
    assert_eq!(
        compatible_assignment(
            &nvidia_candidate(3, "nvidia:GPU-a", 2, &["hevc_cuvid"]),
            Some(&requirement),
            &conflicts,
        ),
        CandidateCompatibility::Incompatible
    );
}

/// The capability's `hardware` column is the live token. A descriptor still present
/// in `extra` while the token has gone means the worker no longer holds that
/// device, so honoring the descriptor would assign a device nothing verifies.
#[test]
fn vaapi_requirement_rejects_a_device_the_candidate_no_longer_advertises() {
    let requirement = VideoHardwareRequirement::vaapi("hevc_vaapi", None);
    let mut stale = vaapi_candidate(1, "0000:f4:00.0", 2, &["hevc_vaapi"], &["hevc"]);
    stale.hardware.clear();

    assert_eq!(
        compatible_assignment(&stale, Some(&requirement), &HashSet::new()),
        CandidateCompatibility::Incompatible
    );
}

/// Capability is probe-proven per codec (ADR 0052 §2), so an encoder or a decode
/// codec the device never proved must not be scheduled onto it. The VAAPI
/// descriptor lists decode *codecs*, not decoder names, because `-hwaccel vaapi`
/// has none.
#[test]
fn vaapi_requirement_requires_the_probed_encoder_and_decode_codec() {
    let conflicts = HashSet::new();
    let decode_hevc = VideoHardwareRequirement::vaapi("hevc_vaapi", Some("hevc".to_owned()));

    assert_eq!(
        compatible_assignment(
            &vaapi_candidate(1, "0000:f4:00.0", 2, &["av1_vaapi"], &["hevc"]),
            Some(&VideoHardwareRequirement::vaapi("hevc_vaapi", None)),
            &conflicts,
        ),
        CandidateCompatibility::Incompatible,
        "an unproven encoder must not be scheduled"
    );
    assert_eq!(
        compatible_assignment(
            &vaapi_candidate(2, "0000:f4:00.0", 2, &["hevc_vaapi"], &["h264", "av1"]),
            Some(&decode_hevc),
            &conflicts,
        ),
        CandidateCompatibility::Incompatible,
        "an unproven decode codec must not be scheduled"
    );
    assert_eq!(
        compatible_assignment(
            &vaapi_candidate(3, "0000:f4:00.0", 2, &["hevc_vaapi"], &["h264", "hevc"]),
            Some(&decode_hevc),
            &conflicts,
        ),
        CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::vaapi(
            "vaapi:pci-0000:f4:00.0",
            "0000:f4:00.0"
        )))
    );
}

/// A source codec VAAPI *can* decode but no live device probed is not a per-file
/// planning fact — another device could have it, and the same file is fine once one
/// appears — so the transcode ticket alone absorbs the outcome and the job keeps
/// running. Projection returns no candidates rather than an error (ADR 0049 §6), the
/// selector reports `NoEligibleWorker`, and that class is one of exactly two
/// `record_pre_lease_ticket_failure` accepts, so the failure lands on the ticket and
/// consumes an attempt.
///
/// It is also distinguished from device saturation: an empty candidate slate is not
/// "all candidates at capacity", so this does not silently become a capacity wait.
///
/// The spec §6 wording for this outcome is "a ticket-scoped `MissingCapability`". The
/// class the pre-lease path actually records is `NoEligibleWorker` — what ADR 0049 §10
/// specifies for a requirement no durable descriptor ever matched, and what the NVIDIA
/// slice records in the same situation. Recording `MissingCapability` would need a
/// class `pre_lease_failure_reason` rejects, and adding it would change NVIDIA
/// behavior, which this slice must not do.
#[test]
fn a_decode_codec_no_live_device_probed_fails_only_its_own_ticket() {
    let requirement = VideoHardwareRequirement::vaapi("hevc_vaapi", Some("av1".to_owned()));
    let conflicts = HashSet::new();
    let candidates = vec![
        vaapi_candidate(1, "0000:03:00.0", 2, &["hevc_vaapi"], &["h264", "hevc"]),
        vaapi_candidate(2, "0000:f4:00.0", 2, &["hevc_vaapi"], &["h264", "hevc"]),
    ];

    for candidate in &candidates {
        assert_eq!(
            compatible_assignment(candidate, Some(&requirement), &conflicts),
            CandidateCompatibility::Incompatible,
            "a codec gap is a mismatch, never a repository fault"
        );
    }

    let projected: Vec<WorkerView> = Vec::new();
    assert!(
        !all_candidates_at_capacity(&projected),
        "an empty slate is a capability gap, not a saturated device"
    );
    let error = LeastLoadedWorkerSelector
        .select(OperationKind::TranscodeVideo, &projected)
        .unwrap_err();

    assert_eq!(
        selector_failure_class(&error).unwrap(),
        voom_core::FailureClass::NoEligibleWorker
    );
}

/// The acceptance host has one render node (spec §10), so cross-device assignment
/// is proven here rather than by a real-media two-device run: each candidate must
/// be assigned its own device and never a sibling's, because the worker validates
/// the assignment against the device it bound and would reject a foreign one.
#[test]
fn a_vaapi_assignment_never_names_another_devices_address() {
    let requirement = VideoHardwareRequirement::vaapi("hevc_vaapi", None);
    let conflicts = HashSet::new();
    let devices = ["0000:03:00.0", "0000:f4:00.0"];

    for (index, address) in devices.iter().enumerate() {
        let candidate = vaapi_candidate(index as u64 + 1, address, 2, &["hevc_vaapi"], &["hevc"]);
        assert_eq!(
            compatible_assignment(&candidate, Some(&requirement), &conflicts),
            CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::vaapi(
                format!("vaapi:pci-{address}"),
                (*address).to_owned()
            ))),
            "worker {} must be assigned {address} and nothing else",
            index + 1
        );
    }
}

/// Duplicate live workers for one device cannot multiply capacity (ADR 0049 §6),
/// and a disagreement about the declaration quarantines the device rather than
/// letting the scheduler pick a number.
#[test]
fn conflicting_vaapi_capacity_declarations_quarantine_the_device() {
    let candidates = vec![
        vaapi_candidate(1, "0000:f4:00.0", 2, &["hevc_vaapi"], &["hevc"]),
        vaapi_candidate(2, "0000:f4:00.0", 3, &["hevc_vaapi"], &["hevc"]),
        vaapi_candidate(3, "0000:03:00.0", 2, &["hevc_vaapi"], &["hevc"]),
    ];

    let conflicts = conflicting_accelerator_tokens(&candidates);

    assert_eq!(
        conflicts,
        HashSet::from(["vaapi:pci-0000:f4:00.0".to_owned()])
    );
    assert_eq!(
        compatible_assignment(
            &candidates[0],
            Some(&VideoHardwareRequirement::vaapi("hevc_vaapi", None)),
            &conflicts,
        ),
        CandidateCompatibility::Incompatible,
        "a quarantined device receives no work"
    );
}

/// Equal load resolves by worker id so independent schedulers make the same choice
/// from the same snapshot, and the assignment map must follow the chosen worker to
/// its own device.
#[test]
fn equal_vaapi_load_selects_the_lowest_worker_id_and_its_own_device() {
    let requirement = VideoHardwareRequirement::vaapi("hevc_vaapi", None);
    let conflicts = HashSet::new();
    let mut workers = Vec::new();
    let mut assignments = HashMap::new();
    for (worker_id, address) in [(7_u64, "0000:03:00.0"), (4, "0000:f4:00.0")] {
        let candidate = vaapi_candidate(worker_id, address, 2, &["hevc_vaapi"], &["hevc"]);
        let CandidateCompatibility::Compatible(Some(assignment)) =
            compatible_assignment(&candidate, Some(&requirement), &conflicts)
        else {
            panic!("both devices are eligible");
        };
        assignments.insert(candidate.worker_id, assignment);
        workers.push(WorkerView {
            worker_id: candidate.worker_id,
            supports: vec![OperationKind::TranscodeVideo],
            active_leases: 0,
            max_parallel: candidate.max_parallel,
        });
    }

    let selected = LeastLoadedWorkerSelector
        .select(OperationKind::TranscodeVideo, &workers)
        .unwrap();

    assert_eq!(selected, WorkerId(4));
    assert_eq!(
        assignments.get(&selected),
        Some(&VideoHardwareAssignment::vaapi(
            "vaapi:pci-0000:f4:00.0",
            "0000:f4:00.0"
        ))
    );
}

/// Recovery tokens are how a dead worker's device is reclaimed, so the token has
/// to be read from whichever backend the assignment carries.
#[test]
fn vaapi_assignment_contributes_its_recovery_token() {
    let mut payload = serde_json::json!({});
    let mut recovery_tokens = Vec::new();
    let assignment = VideoHardwareAssignment::vaapi("vaapi:pci-0000:03:00.0", "0000:03:00.0");

    let applied =
        apply_hardware_assignment(&mut payload, Some(&assignment), &mut recovery_tokens).unwrap();

    assert!(applied);
    assert_eq!(recovery_tokens, vec!["vaapi:pci-0000:03:00.0".to_owned()]);
    assert_eq!(payload["hardware_assignment"]["backend"], "vaapi");
}

#[test]
fn videotoolbox_requirement_requires_exact_encoder_codec_and_pixel_format() {
    let requirement = VideoHardwareRequirement::video_toolbox(
        "hevc_videotoolbox",
        Some(VideoToolboxDecodeRequirement {
            codec: "hevc".to_owned(),
            pixel_format: "yuv420p10le".to_owned(),
        }),
    );
    let candidate = videotoolbox_candidate(
        1,
        "videotoolbox:host-a",
        4,
        &["h264_videotoolbox", "hevc_videotoolbox"],
        &serde_json::json!([
            {"codec": "hevc", "pixel_formats": ["yuv420p", "yuv420p10le"]}
        ]),
    );
    let wrong_format = VideoHardwareRequirement::video_toolbox(
        "hevc_videotoolbox",
        Some(VideoToolboxDecodeRequirement {
            codec: "hevc".to_owned(),
            pixel_format: "p010le".to_owned(),
        }),
    );

    assert_eq!(
        compatible_assignment(&candidate, Some(&requirement), &HashSet::new()),
        CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::video_toolbox(
            "videotoolbox:host-a",
            "host-a"
        )))
    );
    assert_eq!(
        compatible_assignment(&candidate, Some(&wrong_format), &HashSet::new()),
        CandidateCompatibility::Incompatible
    );
    assert_eq!(
        compatible_assignment(
            &nvidia_candidate(2, "nvidia:GPU-a", 2, &["hevc_cuvid"]),
            Some(&requirement),
            &HashSet::new(),
        ),
        CandidateCompatibility::Incompatible
    );
}

#[test]
fn conflicting_capacity_declarations_quarantine_the_token() {
    let candidates = vec![
        nvidia_candidate(1, "nvidia:GPU-a", 2, &["h264_cuvid"]),
        nvidia_candidate(2, "nvidia:GPU-a", 3, &["h264_cuvid"]),
        nvidia_candidate(3, "nvidia:GPU-b", 2, &["h264_cuvid"]),
    ];

    let conflicts = conflicting_accelerator_tokens(&candidates);

    assert_eq!(conflicts, HashSet::from(["nvidia:GPU-a".to_owned()]));
}

/// ADR 0049 §6: one worker's descriptor never breaks projection for the fleet.
/// A descriptor this build cannot read — a rolling upgrade meeting a backend tag
/// from a newer worker, which `deny_unknown_fields` makes the ordinary case —
/// excludes that candidate and nothing else. It must be `Incompatible` rather than
/// an error, and equally must not read as "no accelerator": a device-bound worker
/// passing as unaccelerated is the ADR 0049 §5 hazard this sits between.
#[test]
fn an_unreadable_accelerator_descriptor_excludes_only_that_candidate() {
    let unreadable = WorkerOperationCandidate {
        worker_id: WorkerId(1),
        active_leases: 0,
        max_parallel: 1,
        hardware: vec!["nvidia:GPU-a".to_owned()],
        capability_extra: vec![serde_json::json!({"accelerator": {"hardware_token": 7}})],
    };
    let requirement = VideoHardwareRequirement::nvidia("hevc_nvenc", None);

    let compatibility = compatible_assignment(&unreadable, Some(&requirement), &HashSet::new());

    assert!(matches!(
        compatibility,
        CandidateCompatibility::Incompatible
    ));

    // And it is excluded from the conflict survey rather than failing it, so a
    // healthy sibling on the same token is still surveyed.
    let conflicts = conflicting_accelerator_tokens(&[unreadable]);
    assert!(conflicts.is_empty());
}

/// The exclusion is not a blanket pass: a software requirement must still not be
/// satisfied by a worker whose descriptor could not be read, or an unreadable
/// device-bound worker would pick up software work.
#[test]
fn an_unreadable_descriptor_does_not_satisfy_a_software_requirement() {
    let unreadable = WorkerOperationCandidate {
        worker_id: WorkerId(1),
        active_leases: 0,
        max_parallel: 1,
        hardware: vec!["nvidia:GPU-a".to_owned()],
        capability_extra: vec![serde_json::json!({"accelerator": {"hardware_token": 7}})],
    };
    let requirement = VideoHardwareRequirement::software();

    let compatibility = compatible_assignment(&unreadable, Some(&requirement), &HashSet::new());

    assert!(matches!(
        compatibility,
        CandidateCompatibility::Incompatible
    ));
}

/// Dispatch revalidates endpoint identity before acquiring the lease, not just at run
/// preflight: `candidate_workers` rebuilds the *live* runtime registry for any
/// device-bound requirement and drops every candidate missing from it, and that
/// happens before `try_spawn_dispatch` reaches `acquire_lease_with_retry`. So a device
/// whose supervisor died between preflight and dispatch is never leased — the stale
/// row alone must not be enough.
///
/// `127.0.0.1:1` is a closed privileged port, standing in for the endpoint a
/// hard-killed `run-local` leaves behind.
#[tokio::test]
async fn a_vaapi_candidate_with_a_dead_endpoint_is_dropped_before_any_lease() {
    let (cp, _tmp) = crate::cases::cp().await;
    let operation = TicketOperation::new("transcode_video").unwrap();
    let worker = cp
        .register_worker(voom_store::repo::execution::workers::NewWorker {
            name: "vaapi-dead-endpoint".to_owned(),
            kind: voom_core::WorkerKind::Synthetic,
            registered_at: cp.clock().now(),
            node_id: None,
        })
        .await
        .unwrap();
    cp.record_capability(voom_store::repo::execution::workers::NewCapability {
        worker_id: worker.id,
        operation: operation.clone(),
        codecs: Vec::new(),
        hardware: vec!["vaapi:pci-0000:f4:00.0".to_owned()],
        artifact_access: Vec::new(),
        extra: serde_json::json!({
            "endpoint": "127.0.0.1:1",
            "secret": "vaapi-dead-secret",
            "accelerator": {
                "backend": "vaapi",
                "pci_address": "0000:f4:00.0",
                "device_name": "AMD Radeon 8060S Graphics",
                "driver_version": "Mesa Gallium 26.1.5 radeonsi",
                "encoders": ["hevc_vaapi"],
                "decoders": ["hevc"],
                "max_sessions": 2
            }
        }),
    })
    .await
    .unwrap();
    cp.record_grant(voom_store::repo::execution::workers::NewGrant {
        worker_id: worker.id,
        can_execute: vec![operation.clone()],
        can_access_read: Vec::new(),
        can_access_write: Vec::new(),
        denies: Vec::new(),
        max_parallel: serde_json::json!({"transcode_video": 2}),
    })
    .await
    .unwrap();
    let payload = serde_json::json!({
        "resolved_profile": {
            "name": "vaapi-hevc",
            "target_codec": "hevc",
            "encoder": "hevc_vaapi",
            "qp": 24
        },
        "source_video_codec": "hevc"
    });
    let executor = WorkflowExecutor::with_options(
        cp.clone(),
        WorkerRuntimeRegistry::new(),
        crate::workflow::execution::executor::WorkflowExecutorOptions::for_tests(),
    );

    let projected = executor
        .candidate_workers(
            OperationKind::TranscodeVideo,
            &payload,
            &HashMap::new(),
            &mut HashMap::new(),
            &mut None,
        )
        .await
        .unwrap();

    assert!(
        projected.workers.is_empty(),
        "a device whose endpoint fails the liveness probe must not be a candidate"
    );
    assert!(projected.assignments.is_empty());
    let leases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(leases, 0, "projection must not have leased the device");
}

/// A profile's requirement comes from its encoder's `VideoEncoderBackend`, so an
/// accelerated encoder can never be handed a software requirement and dispatched to
/// a CPU worker. Deriving it from a string comparison against one encoder name gave
/// `hevc_vaapi` a *software* requirement — the silent software fallback issue #409
/// forbids — and would do the same to a fifth backend.
#[test]
fn a_vaapi_profile_requires_its_own_backend_not_software() {
    let payload = serde_json::json!({
        "resolved_profile": {
            "name": "vaapi-hevc",
            "target_codec": "hevc",
            "encoder": "hevc_vaapi",
            "qp": 24
        },
        "source_video_codec": "h264"
    });

    let requirement = video_hardware_requirement(OperationKind::TranscodeVideo, &payload)
        .unwrap()
        .unwrap();

    assert_eq!(
        requirement,
        VideoHardwareRequirement::vaapi("hevc_vaapi", None)
    );
}

/// A `vaapi`-decode profile must carry the decode requirement too, so projection
/// can refuse a device that never proved a decoder for the source codec. The
/// requirement names the canonical source codec, because a VAAPI descriptor lists
/// codecs rather than decoder names.
#[test]
fn a_vaapi_decode_profile_requires_the_source_codec_as_its_decoder() {
    let payload = serde_json::json!({
        "resolved_profile": {
            "name": "vaapi-hevc",
            "target_codec": "hevc",
            "encoder": "hevc_vaapi",
            "qp": 24,
            "decode": {"backend": "vaapi"}
        },
        "source_video_codec": "H265"
    });

    let requirement = video_hardware_requirement(OperationKind::TranscodeVideo, &payload)
        .unwrap()
        .unwrap();

    assert_eq!(
        requirement,
        VideoHardwareRequirement::vaapi("hevc_vaapi", Some("hevc".to_owned()))
    );
}

/// VAAPI decodes only `h264`, `hevc`, and `av1`. A source codec outside that set
/// must fail loud at requirement derivation rather than produce a requirement no
/// device can satisfy, which would read as a busy fleet.
#[test]
fn a_vaapi_decode_profile_rejects_a_source_codec_vaapi_cannot_decode() {
    let payload = serde_json::json!({
        "resolved_profile": {
            "name": "vaapi-hevc",
            "target_codec": "hevc",
            "encoder": "hevc_vaapi",
            "qp": 24,
            "decode": {"backend": "vaapi"}
        },
        "source_video_codec": "vp9"
    });

    let error = video_hardware_requirement(OperationKind::TranscodeVideo, &payload).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("VAAPI decode does not support source video codec `vp9`"),
        "the diagnostic must name the codec: {error}"
    );
}

/// A software profile keeps requiring an unaccelerated worker, unchanged by the
/// backend-derived rule.
#[test]
fn a_software_profile_still_requires_an_unaccelerated_worker() {
    let payload = serde_json::json!({
        "resolved_profile": {
            "name": "default-hevc",
            "target_codec": "hevc",
            "encoder": "libx265",
            "crf": 23,
            "preset": "medium"
        },
        "source_video_codec": "h264"
    });

    let requirement = video_hardware_requirement(OperationKind::TranscodeVideo, &payload)
        .unwrap()
        .unwrap();

    assert_eq!(requirement, VideoHardwareRequirement::software());
}

#[test]
fn hardware_requirement_uses_profile_and_source_codec() {
    let payload = serde_json::json!({
        "resolved_profile": {
            "name": "gpu",
            "target_codec": "hevc",
            "encoder": "hevc_nvenc",
            "cq": 22,
            "preset": "p5",
            "decode": {"backend": "nvidia"}
        },
        "source_video_codec": "h264"
    });

    let requirement = video_hardware_requirement(OperationKind::TranscodeVideo, &payload)
        .unwrap()
        .unwrap();

    assert_eq!(
        requirement,
        VideoHardwareRequirement::nvidia("hevc_nvenc", Some("h264_cuvid".to_owned()))
    );
}

#[test]
fn videotoolbox_requirement_uses_source_codec_and_pixel_format() {
    let payload = serde_json::json!({
        "resolved_profile": {
            "name": "hevc-videotoolbox",
            "target_codec": "hevc",
            "encoder": "hevc_videotoolbox",
            "bitrate_kbps": 8000,
            "preset": "default",
            "codec_profile": "main10",
            "pixel_format": "yuv420p10le",
            "decode": {"backend": "video_toolbox"}
        },
        "source_video_codec": "hevc",
        "source_video_pixel_format": "yuv420p10le"
    });

    let requirement = video_hardware_requirement(OperationKind::TranscodeVideo, &payload)
        .unwrap()
        .unwrap();

    assert_eq!(
        requirement,
        VideoHardwareRequirement::video_toolbox(
            "hevc_videotoolbox",
            Some(VideoToolboxDecodeRequirement {
                codec: "hevc".to_owned(),
                pixel_format: "yuv420p10le".to_owned(),
            })
        )
    );
}
