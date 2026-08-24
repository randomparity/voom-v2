use voom_control_plane::execution::PreLeaseFailureOutcome;
use voom_control_plane::policy::{ArtifactVerificationView, BackupEvidence, ProgressCountsView};
use voom_control_plane::transcode::ResolvedProfile;
use voom_control_plane::workers::{LocalWorkerKind, RegisteredNode};
use voom_control_plane::workflow::{EffectiveTiming, WorkflowTicketPayloadError};

// The bundled control-plane remux dispatcher (`RemuxDispatcher`,
// `RemuxProgressSink`) was removed with the T8 sweep: remux tickets execute on
// their storage owner's agent via `media_dispatch` envelopes. Downstream
// capability now rests on the node/local-worker registration surface instead
// of an in-process remux dispatcher trait.
#[test]
fn capability_contract_types_are_nameable_downstream() {
    fn assert_nameable<T>() {}

    assert_nameable::<PreLeaseFailureOutcome>();
    assert_nameable::<ArtifactVerificationView>();
    assert_nameable::<BackupEvidence>();
    assert_nameable::<ProgressCountsView>();
    assert_nameable::<ResolvedProfile>();
    assert_nameable::<EffectiveTiming>();
    assert_nameable::<WorkflowTicketPayloadError>();
    assert_nameable::<LocalWorkerKind>();
    assert_nameable::<RegisteredNode>();
}
