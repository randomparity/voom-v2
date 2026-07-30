use voom_worker_protocol::{
    NvidiaVideoHardwareAssignment, VaapiVideoHardwareAssignment, VideoHardwareAssignment,
    vaapi_hardware_token,
};

use super::hardware_evidence;

const PCI_ADDRESS: &str = "0000:f4:00.0";

/// Issue #409 requires durable evidence of which device produced an artifact, and a
/// VAAPI device's identity *is* its PCI address (ADR 0051 §1) — there is no UUID to
/// record. So the succeeded event must carry the backend and the same
/// `vaapi:pci-<addr>` token the scheduler leased, and must leave
/// `hardware_device_uuid` absent rather than smuggle a non-UUID into that column,
/// where a later reader would take it for one.
#[test]
fn a_vaapi_assignment_records_the_backend_and_token_but_no_uuid() {
    let assignment = VideoHardwareAssignment::Vaapi(VaapiVideoHardwareAssignment {
        hardware_token: vaapi_hardware_token(PCI_ADDRESS),
        pci_address: PCI_ADDRESS.to_owned(),
    });

    let (backend, token, uuid) = hardware_evidence(Some(&assignment));

    assert_eq!(backend.as_deref(), Some("vaapi"));
    assert_eq!(token.as_deref(), Some("vaapi:pci-0000:f4:00.0"));
    assert_eq!(
        token.as_deref(),
        Some(vaapi_hardware_token(PCI_ADDRESS).as_str()),
        "the recorded token must be the one formatter every other site uses, or the \
         evidence names a device the scheduler never leased"
    );
    assert!(
        uuid.is_none(),
        "a PCI address is not a UUID; recording one here would misidentify the device"
    );
}

/// The VAAPI arm must not have cost NVIDIA its UUID. NVIDIA identity is the UUID
/// (ADR 0049 §2), so that column staying populated is what keeps the two backends'
/// evidence distinguishable in one durable event stream.
#[test]
fn an_nvidia_assignment_still_records_its_device_uuid() {
    let assignment = VideoHardwareAssignment::Nvidia(NvidiaVideoHardwareAssignment {
        hardware_token: "nvidia:GPU-11111111-2222-3333-4444-555555555555".to_owned(),
        device_uuid: "GPU-11111111-2222-3333-4444-555555555555".to_owned(),
    });

    let (backend, token, uuid) = hardware_evidence(Some(&assignment));

    assert_eq!(backend.as_deref(), Some("nvidia"));
    assert_eq!(
        token.as_deref(),
        Some("nvidia:GPU-11111111-2222-3333-4444-555555555555")
    );
    assert_eq!(
        uuid.as_deref(),
        Some("GPU-11111111-2222-3333-4444-555555555555")
    );
}

/// Software work records no hardware evidence at all, and neither does a transcode
/// with no assignment. `hardware_backend` being absent is therefore the signal that
/// no accelerator was involved — which is why an absent `hardware_device_uuid`
/// cannot be allowed to mean the same thing.
#[test]
fn software_and_unassigned_transcodes_record_no_hardware_evidence() {
    for assignment in [None, Some(VideoHardwareAssignment::software())] {
        let (backend, token, uuid) = hardware_evidence(assignment.as_ref());

        assert!(backend.is_none(), "{assignment:?}");
        assert!(token.is_none(), "{assignment:?}");
        assert!(uuid.is_none(), "{assignment:?}");
    }
}
