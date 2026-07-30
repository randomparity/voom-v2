use voom_core::VoomError;
use voom_store::repo::workers::{WorkerOperationCandidate, WorkerOperationCapability};
use voom_worker_protocol::NvidiaVideoAcceleratorDescriptor;

pub(crate) fn candidate_accelerator_descriptor(
    candidate: &WorkerOperationCandidate,
) -> Result<Option<NvidiaVideoAcceleratorDescriptor>, VoomError> {
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
) -> Result<Option<NvidiaVideoAcceleratorDescriptor>, VoomError> {
    let Some(value) = capability.extra.get("accelerator") else {
        return Ok(None);
    };
    parse_descriptor(value, "historical worker capability").map(Some)
}

/// A stored descriptor is the untagged NVIDIA struct (pre-#409 rows are durable and
/// carry no tag) or a `backend`-tagged struct for any later backend, so the tag's
/// presence is what tells them apart.
///
/// A tagged descriptor is well-formed but not NVIDIA, and saying it is malformed
/// would send an operator looking for corruption that is not there.
fn parse_descriptor(
    value: &serde_json::Value,
    context: &str,
) -> Result<NvidiaVideoAcceleratorDescriptor, VoomError> {
    if let Some(backend) = value.get("backend").and_then(serde_json::Value::as_str) {
        return Err(VoomError::Config(format!(
            "{context} advertises a `{backend}` accelerator descriptor, which this code path \
             cannot consume; only NVIDIA descriptors are wired into scheduling"
        )));
    }
    serde_json::from_value(value.clone()).map_err(|error| {
        VoomError::Config(format!(
            "{context} has a malformed NVIDIA accelerator descriptor: {error}"
        ))
    })
}
