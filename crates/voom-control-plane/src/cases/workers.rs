//! Worker-lifecycle use cases. Each method opens a transaction, calls the
//! `WorkerRepo` `_in_tx` form, emits the matching event via
//! `EventRepo::append_in_tx`, then commits.

use time::OffsetDateTime;
use voom_core::{VoomError, WorkerId};
use voom_events::payload::{
    WorkerCapabilityRecordedPayload, WorkerGrantRecordedPayload, WorkerRegisteredPayload,
    WorkerRetiredPayload,
};
use voom_events::{Event, EventKind, SubjectType};
use voom_store::repo::workers::{
    Capability, Grant, NewCapability, NewGrant, NewWorker, Worker, WorkerRepo,
};

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx};

impl ControlPlane {
    /// Register a worker and emit `worker.registered`.
    ///
    /// # Errors
    /// Propagates `WorkerRepo::register_in_tx` and event-append errors.
    pub async fn register_worker(&self, input: NewWorker) -> Result<Worker, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let worker = self.workers.register_in_tx(&mut tx, input.clone()).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::WorkerRegistered,
            SubjectType::Worker,
            Some(worker.id.0),
            input.registered_at,
            Event::WorkerRegistered(WorkerRegisteredPayload {
                worker_id: worker.id.0,
                name: worker.name.clone(),
                kind: worker.kind.as_str().to_owned(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(worker)
    }

    /// Record a worker capability and emit `worker.capability_recorded`.
    ///
    /// # Errors
    /// Propagates `WorkerRepo::record_capability_in_tx` and event-append errors.
    pub async fn record_capability(&self, input: NewCapability) -> Result<Capability, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let worker_id = input.worker_id;
        let operation = input.operation.clone();
        let cap = self.workers.record_capability_in_tx(&mut tx, input).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::WorkerCapabilityRecorded,
            SubjectType::Worker,
            Some(worker_id.0),
            self.clock().now(),
            Event::WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload {
                worker_id: worker_id.0,
                capability_id: cap.id,
                operation,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(cap)
    }

    /// Record a worker grant and emit `worker.grant_recorded`.
    ///
    /// # Errors
    /// Propagates `WorkerRepo::record_grant_in_tx` and event-append errors.
    pub async fn record_grant(&self, input: NewGrant) -> Result<Grant, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let worker_id = input.worker_id;
        let grant = self.workers.record_grant_in_tx(&mut tx, input).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::WorkerGrantRecorded,
            SubjectType::Worker,
            Some(worker_id.0),
            self.clock().now(),
            Event::WorkerGrantRecorded(WorkerGrantRecordedPayload {
                worker_id: worker_id.0,
                grant_id: grant.id,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(grant)
    }

    /// Retire a worker and emit `worker.retired`.
    ///
    /// # Errors
    /// Propagates `WorkerRepo::retire_in_tx` and event-append errors.
    pub async fn retire_worker(
        &self,
        id: WorkerId,
        expected_epoch: u64,
        now: OffsetDateTime,
    ) -> Result<Worker, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let worker = self
            .workers
            .retire_in_tx(&mut tx, id, expected_epoch, now)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::WorkerRetired,
            SubjectType::Worker,
            Some(id.0),
            now,
            Event::WorkerRetired(WorkerRetiredPayload { worker_id: id.0 }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(worker)
    }
}

#[cfg(test)]
#[path = "workers_test.rs"]
mod tests;
