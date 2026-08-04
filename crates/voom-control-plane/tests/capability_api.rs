use async_trait::async_trait;
use voom_control_plane::execution::PreLeaseFailureOutcome;
use voom_control_plane::policy::{ArtifactVerificationView, BackupEvidence, ProgressCountsView};
use voom_control_plane::remux::{RemuxDispatcher, RemuxProgressSink};
use voom_control_plane::transcode::ResolvedProfile;
use voom_control_plane::workflow::{EffectiveTiming, WorkflowTicketPayloadError};
use voom_core::VoomError;
use voom_worker_protocol::{RemuxRequest, RemuxResult};

struct ExternalRemuxDispatcher;

#[async_trait]
impl RemuxDispatcher for ExternalRemuxDispatcher {
    async fn dispatch_remux_with_progress(
        &self,
        _request: RemuxRequest,
        _progress: &mut dyn RemuxProgressSink,
    ) -> Result<RemuxResult, VoomError> {
        Err(VoomError::Internal(
            "compile-only dispatcher invoked".to_owned(),
        ))
    }
}

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

    let dispatcher: &dyn RemuxDispatcher = &ExternalRemuxDispatcher;
    let _ = dispatcher;
}
