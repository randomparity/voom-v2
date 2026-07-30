use voom_core::VoomError;
use voom_store::repo::workers::{WorkerOperationCandidate, WorkerOperationCapability};
use voom_worker_protocol::VideoAcceleratorDescriptor;

pub(crate) fn candidate_accelerator_descriptor(
    candidate: &WorkerOperationCandidate,
) -> Result<Option<VideoAcceleratorDescriptor>, VoomError> {
    let mut descriptors = Vec::new();
    for extra in &candidate.capability_extra {
        let Some(value) = extra.get("accelerator") else {
            continue;
        };
        descriptors.push(parse_descriptor(
            value,
            &format!("worker {}", candidate.worker_id.0),
        )?);
    }
    match descriptors.len() {
        0 => Ok(None),
        1 => Ok(descriptors.pop()),
        count => Err(VoomError::Config(format!(
            "worker {} advertises {count} accelerator descriptors for one operation",
            candidate.worker_id.0
        ))),
    }
}

pub(crate) fn historical_accelerator_descriptor(
    capability: &WorkerOperationCapability,
) -> Result<Option<VideoAcceleratorDescriptor>, VoomError> {
    let Some(value) = capability.extra.get("accelerator") else {
        return Ok(None);
    };
    parse_descriptor(value, "historical worker capability").map(Some)
}

/// Every stored descriptor is `backend`-tagged: migration 0031 backfilled `nvidia`
/// onto the pre-#411 rows, so there is no untagged shape left to sniff for.
///
/// A backend this build does not know is a malformed-descriptor error for that one
/// worker; returning `Ok(None)` instead would let a device-bound worker pass as
/// unaccelerated and pick up software work (ADR 0049 §5).
fn parse_descriptor(
    value: &serde_json::Value,
    context: &str,
) -> Result<VideoAcceleratorDescriptor, VoomError> {
    serde_json::from_value(value.clone()).map_err(|error| {
        VoomError::Config(format!(
            "{context} has a malformed video accelerator descriptor: {error}"
        ))
    })
}
