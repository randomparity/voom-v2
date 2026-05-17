//! Worker-lifecycle use cases. Each method opens a transaction, calls the
//! `WorkerRepo` `_in_tx` form, emits the matching event via
//! `EventRepo::append_in_tx`, then commits.

use time::OffsetDateTime;
use voom_core::{VoomError, WorkerId};
use voom_events::payload::{
    WorkerCapabilityRecordedPayload, WorkerGrantRecordedPayload, WorkerRegisteredPayload,
    WorkerRetiredPayload,
};
use voom_events::{Event, EventEnvelope, EventKind, SubjectType};
use voom_store::repo::events::EventRepo;
use voom_store::repo::workers::{
    Capability, Grant, NewCapability, NewGrant, NewWorker, Worker, WorkerRepo,
};

use crate::ControlPlane;

impl ControlPlane {
    /// Register a worker and emit `worker.registered`.
    ///
    /// # Errors
    /// Propagates `WorkerRepo::register_in_tx` and event-append errors.
    pub async fn register_worker(&self, input: NewWorker) -> Result<Worker, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let worker = self.workers.register_in_tx(&mut tx, input.clone()).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::WorkerRegistered,
                    occurred_at: input.registered_at,
                    subject_type: SubjectType::Worker,
                    subject_id: Some(worker.id.0),
                    trace_id: None,
                    payload: Event::WorkerRegistered(WorkerRegisteredPayload {
                        worker_id: worker.id.0,
                        name: worker.name.clone(),
                        kind: worker.kind.as_str().to_owned(),
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(worker)
    }

    /// Record a worker capability and emit `worker.capability_recorded`.
    ///
    /// # Errors
    /// Propagates `WorkerRepo::record_capability_in_tx` and event-append errors.
    pub async fn record_capability(&self, input: NewCapability) -> Result<Capability, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let worker_id = input.worker_id;
        let operation = input.operation.clone();
        let cap = self.workers.record_capability_in_tx(&mut tx, input).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::WorkerCapabilityRecorded,
                    occurred_at: self.clock().now(),
                    subject_type: SubjectType::Worker,
                    subject_id: Some(worker_id.0),
                    trace_id: None,
                    payload: Event::WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload {
                        worker_id: worker_id.0,
                        capability_id: cap.id,
                        operation,
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(cap)
    }

    /// Record a worker grant and emit `worker.grant_recorded`.
    ///
    /// # Errors
    /// Propagates `WorkerRepo::record_grant_in_tx` and event-append errors.
    pub async fn record_grant(&self, input: NewGrant) -> Result<Grant, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let worker_id = input.worker_id;
        let grant = self.workers.record_grant_in_tx(&mut tx, input).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::WorkerGrantRecorded,
                    occurred_at: self.clock().now(),
                    subject_type: SubjectType::Worker,
                    subject_id: Some(worker_id.0),
                    trace_id: None,
                    payload: Event::WorkerGrantRecorded(WorkerGrantRecordedPayload {
                        worker_id: worker_id.0,
                        grant_id: grant.id,
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
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
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let worker = self
            .workers
            .retire_in_tx(&mut tx, id, expected_epoch, now)
            .await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::WorkerRetired,
                    occurred_at: now,
                    subject_type: SubjectType::Worker,
                    subject_id: Some(id.0),
                    trace_id: None,
                    payload: Event::WorkerRetired(WorkerRetiredPayload { worker_id: id.0 }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(worker)
    }
}

#[cfg(test)]
#[path = "workers_test.rs"]
mod tests;
