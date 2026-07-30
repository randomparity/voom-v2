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

#[test]
fn software_requirement_excludes_device_bound_workers() {
    let requirement = VideoHardwareRequirement::software();
    let conflicts = HashSet::new();

    assert_eq!(
        compatible_assignment(&software_candidate(1), Some(&requirement), &conflicts).unwrap(),
        CandidateCompatibility::Compatible(None)
    );
    assert_eq!(
        compatible_assignment(
            &nvidia_candidate(2, "nvidia:GPU-a", 2, &["h264_cuvid"]),
            Some(&requirement),
            &conflicts,
        )
        .unwrap(),
        CandidateCompatibility::Incompatible
    );
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
        compatible_assignment(&without_av1, Some(&requirement), &conflicts).unwrap(),
        CandidateCompatibility::Incompatible
    );
    assert_eq!(
        compatible_assignment(&with_av1, Some(&requirement), &conflicts).unwrap(),
        CandidateCompatibility::Compatible(Some(VideoHardwareAssignment::nvidia(
            "nvidia:GPU-b",
            "GPU-b"
        )))
    );
}

/// Device selection for the VAAPI backend is not wired yet, so a VAAPI
/// requirement must abort projection rather than quietly match no device — a
/// silent no-candidates outcome reads like a busy fleet, not a coding gap.
#[test]
fn vaapi_requirement_fails_candidate_projection_until_device_selection_exists() {
    let requirement = VideoHardwareRequirement::vaapi("hevc_vaapi", None);

    let error = compatible_assignment(&software_candidate(1), Some(&requirement), &HashSet::new())
        .unwrap_err();

    assert!(
        error.to_string().contains("VAAPI hardware requirement"),
        "the diagnostic must name the unwired backend: {error}"
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
fn conflicting_capacity_declarations_quarantine_the_token() {
    let candidates = vec![
        nvidia_candidate(1, "nvidia:GPU-a", 2, &["h264_cuvid"]),
        nvidia_candidate(2, "nvidia:GPU-a", 3, &["h264_cuvid"]),
        nvidia_candidate(3, "nvidia:GPU-b", 2, &["h264_cuvid"]),
    ];

    let conflicts = conflicting_accelerator_tokens(&candidates).unwrap();

    assert_eq!(conflicts, HashSet::from(["nvidia:GPU-a".to_owned()]));
}

#[test]
fn malformed_accelerator_descriptor_fails_candidate_projection() {
    let candidate = WorkerOperationCandidate {
        worker_id: WorkerId(1),
        active_leases: 0,
        max_parallel: 1,
        hardware: vec!["nvidia:GPU-a".to_owned()],
        capability_extra: vec![serde_json::json!({"accelerator": {"hardware_token": 7}})],
    };
    let requirement = VideoHardwareRequirement::nvidia("hevc_nvenc", None);

    let error = compatible_assignment(&candidate, Some(&requirement), &HashSet::new()).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("malformed NVIDIA accelerator descriptor")
    );
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
