use std::collections::HashMap;
use std::sync::Arc;

use voom_core::{VoomError, WorkerId};
use voom_worker_protocol::{ClientHandle, WorkerCredentials};

#[derive(Debug, Clone)]
pub struct WorkerRuntime {
    pub client: Arc<dyn ClientHandle>,
    pub credentials: WorkerCredentials,
}

#[derive(Debug, Clone, Default)]
pub struct WorkerRuntimeRegistry {
    runtimes: HashMap<WorkerId, WorkerRuntime>,
}

impl WorkerRuntimeRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_in_process_runtime<C>(
        mut self,
        worker_id: WorkerId,
        client: Arc<C>,
        credentials: WorkerCredentials,
    ) -> Self
    where
        C: ClientHandle + 'static,
    {
        self.register_in_process_runtime(worker_id, client, credentials);
        self
    }

    pub fn register_in_process_runtime<C>(
        &mut self,
        worker_id: WorkerId,
        client: Arc<C>,
        credentials: WorkerCredentials,
    ) where
        C: ClientHandle + 'static,
    {
        self.runtimes.insert(
            worker_id,
            WorkerRuntime {
                client,
                credentials,
            },
        );
    }

    pub fn get(&self, worker_id: WorkerId) -> Result<WorkerRuntime, VoomError> {
        self.runtimes
            .get(&worker_id)
            .cloned()
            .ok_or_else(|| VoomError::Config(format!("missing runtime for worker {worker_id}")))
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
