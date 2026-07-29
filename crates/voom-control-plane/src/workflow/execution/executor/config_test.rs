use super::*;
use crate::local_worker::NVIDIA_STARTUP_TIMEOUT;

#[test]
fn accelerator_recovery_outlives_full_nvidia_startup_budget() {
    assert!(
        WorkflowQueueOptions::default().accelerator_unavailable_timeout > NVIDIA_STARTUP_TIMEOUT
    );
}
