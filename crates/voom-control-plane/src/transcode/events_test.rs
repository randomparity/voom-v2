use super::*;

#[test]
fn videotoolbox_evidence_uses_generic_resource_id_without_device_uuid() {
    let assignment =
        VideoHardwareAssignment::video_toolbox("videotoolbox:0123456789abcdef", "0123456789abcdef");

    let (backend, token, device_uuid, resource_id) = hardware_evidence(Some(&assignment));

    assert_eq!(backend.as_deref(), Some("video_toolbox"));
    assert_eq!(token.as_deref(), Some("videotoolbox:0123456789abcdef"));
    assert_eq!(device_uuid, None);
    assert_eq!(resource_id.as_deref(), Some("0123456789abcdef"));
}
