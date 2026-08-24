//! Executor configuration: timing, queue, artifact-root, and dispatch/stream
//! option structs plus the synthetic workflow job-kind constant.

use std::time::Duration;

#[cfg(test)]
use crate::workflow::execution::executor::WorkflowChaosOptions;

pub(crate) const WORKFLOW_JOB_KIND: &str = "synthetic.workflow";
const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_PROGRESS_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_READY_BATCH_SIZE: u32 = 64;
const DEFAULT_MAX_ATTEMPTS: u32 = 1;
const DEFAULT_CAPACITY_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_CAPACITY_RETRY_TIMEOUT: Duration = Duration::from_mins(1);
const DEFAULT_ACCELERATOR_UNAVAILABLE_TIMEOUT: Duration = Duration::from_mins(15);

#[derive(Debug, Clone)]
pub(crate) struct WorkflowTimingOptions {
    pub lease_ttl: Duration,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
    pub progress_idle_timeout: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowQueueOptions {
    pub ready_batch_size: u32,
    pub max_attempts: u32,
    pub capacity_retry_interval: Duration,
    pub capacity_retry_timeout: Duration,
    pub accelerator_unavailable_timeout: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowDispatchOptions {
    pub timing: WorkflowTimingOptions,
    #[cfg(test)]
    pub chaos: WorkflowChaosOptions,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowStreamOptions {
    pub timing: WorkflowTimingOptions,
    #[cfg(test)]
    pub chaos: WorkflowChaosOptions,
}

impl Default for WorkflowTimingOptions {
    fn default() -> Self {
        Self {
            lease_ttl: DEFAULT_LEASE_TTL,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            heartbeat_timeout: DEFAULT_HEARTBEAT_TIMEOUT,
            progress_idle_timeout: DEFAULT_PROGRESS_IDLE_TIMEOUT,
        }
    }
}

impl WorkflowTimingOptions {
    #[cfg(test)]
    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            lease_ttl: Duration::from_secs(5),
            heartbeat_interval: Duration::from_secs(1),
            heartbeat_timeout: Duration::from_secs(5),
            progress_idle_timeout: Duration::from_secs(5),
        }
    }
}

impl Default for WorkflowQueueOptions {
    fn default() -> Self {
        Self {
            ready_batch_size: DEFAULT_READY_BATCH_SIZE,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            capacity_retry_interval: DEFAULT_CAPACITY_RETRY_INTERVAL,
            capacity_retry_timeout: DEFAULT_CAPACITY_RETRY_TIMEOUT,
            accelerator_unavailable_timeout: DEFAULT_ACCELERATOR_UNAVAILABLE_TIMEOUT,
        }
    }
}

impl WorkflowQueueOptions {
    #[cfg(test)]
    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            capacity_retry_interval: Duration::from_millis(10),
            capacity_retry_timeout: Duration::from_millis(250),
            accelerator_unavailable_timeout: Duration::from_millis(500),
            ..Self::default()
        }
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;

impl WorkflowDispatchOptions {
    #[must_use]
    pub fn stream_options(&self) -> WorkflowStreamOptions {
        WorkflowStreamOptions {
            timing: self.timing.clone(),
            #[cfg(test)]
            chaos: self.chaos.clone(),
        }
    }
}
