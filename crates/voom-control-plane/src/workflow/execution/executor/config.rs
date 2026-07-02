//! Executor configuration: timing, queue, artifact-root, and dispatch/stream
//! option structs plus the synthetic workflow job-kind constant.

use std::path::PathBuf;
use std::time::Duration;

use crate::workflow::execution::executor::WorkflowChaosOptions;

pub(crate) const WORKFLOW_JOB_KIND: &str = "synthetic.workflow";
const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_PROGRESS_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_READY_BATCH_SIZE: u32 = 64;
const DEFAULT_MAX_ATTEMPTS: u32 = 1;

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
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowArtifactRoots {
    pub transcode: OperationArtifactRoots,
    pub remux: OperationArtifactRoots,
    pub audio: OperationArtifactRoots,
    /// Opt-in backup-before-mutation destination root (`--backup-root`). When
    /// `Some`, every mutating operation backs up its source here before
    /// dispatch. `None` disables the gate. See ADR 0025.
    pub backup_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct OperationArtifactRoots {
    pub staging_root: PathBuf,
    pub target_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowDispatchOptions {
    pub timing: WorkflowTimingOptions,
    pub artifact_roots: WorkflowArtifactRoots,
    pub chaos: WorkflowChaosOptions,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowStreamOptions {
    pub timing: WorkflowTimingOptions,
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
            heartbeat_interval: Duration::from_millis(10),
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
        }
    }
}

impl OperationArtifactRoots {
    #[must_use]
    pub fn new(staging_root: PathBuf, target_dir: PathBuf) -> Self {
        Self {
            staging_root,
            target_dir,
        }
    }
}

impl Default for WorkflowArtifactRoots {
    fn default() -> Self {
        Self {
            transcode: OperationArtifactRoots::new(
                PathBuf::from("/tmp/voom/transcode/staging"),
                PathBuf::from("/tmp/voom/transcode/output"),
            ),
            remux: OperationArtifactRoots::new(
                PathBuf::from("/tmp/voom/remux/staging"),
                PathBuf::from("/tmp/voom/remux/output"),
            ),
            audio: OperationArtifactRoots::new(
                PathBuf::from("/tmp/voom/audio/staging"),
                PathBuf::from("/tmp/voom/audio/output"),
            ),
            backup_root: None,
        }
    }
}

impl WorkflowArtifactRoots {
    #[cfg(test)]
    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            transcode: OperationArtifactRoots::new(
                PathBuf::from("/tmp/voom-test/transcode/staging"),
                PathBuf::from("/tmp/voom-test/transcode/output"),
            ),
            remux: OperationArtifactRoots::new(
                PathBuf::from("/tmp/voom-test/remux/staging"),
                PathBuf::from("/tmp/voom-test/remux/output"),
            ),
            audio: OperationArtifactRoots::new(
                PathBuf::from("/tmp/voom-test/audio/staging"),
                PathBuf::from("/tmp/voom-test/audio/output"),
            ),
            backup_root: None,
        }
    }
}

impl WorkflowDispatchOptions {
    #[must_use]
    pub fn stream_options(&self) -> WorkflowStreamOptions {
        WorkflowStreamOptions {
            timing: self.timing.clone(),
            chaos: self.chaos.clone(),
        }
    }
}
