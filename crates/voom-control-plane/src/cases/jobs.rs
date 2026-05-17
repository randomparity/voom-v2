//! Job-lifecycle use cases. Each method opens a transaction, calls the
//! `JobRepo` `_in_tx` form, emits the matching event via
//! `EventRepo::append_in_tx`, then commits.

use time::OffsetDateTime;
use voom_core::{JobId, VoomError};
use voom_events::payload::{
    JobCancelledPayload, JobFailedPayload, JobOpenedPayload, JobSucceededPayload,
};
use voom_events::{Event, EventEnvelope, EventKind, SubjectType};
use voom_store::repo::events::EventRepo;
use voom_store::repo::jobs::{Job, JobRepo, NewJob};

use crate::ControlPlane;

impl ControlPlane {
    /// Open a new job and emit `job.opened` in the same transaction.
    ///
    /// # Errors
    /// Propagates `JobRepo::create_in_tx` and `EventRepo::append_in_tx` errors.
    pub async fn open_job(&self, input: NewJob) -> Result<Job, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let job = self.jobs.create_in_tx(&mut tx, input.clone()).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::JobOpened,
                    occurred_at: input.created_at,
                    subject_type: SubjectType::Job,
                    subject_id: Some(job.id.0),
                    trace_id: None,
                    payload: Event::JobOpened(JobOpenedPayload {
                        job_id: job.id.0,
                        kind: input.kind.clone(),
                        priority: input.priority,
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(job)
    }

    /// Mark a job succeeded and emit `job.succeeded`.
    ///
    /// # Errors
    /// Propagates `JobRepo::succeed_in_tx` and event-append errors.
    pub async fn succeed_job(&self, id: JobId, now: OffsetDateTime) -> Result<Job, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let job = self.jobs.succeed_in_tx(&mut tx, id, now).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::JobSucceeded,
                    occurred_at: now,
                    subject_type: SubjectType::Job,
                    subject_id: Some(id.0),
                    trace_id: None,
                    payload: Event::JobSucceeded(JobSucceededPayload { job_id: id.0 }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(job)
    }

    /// Mark a job failed and emit `job.failed` carrying `reason`.
    ///
    /// # Errors
    /// Propagates `JobRepo::fail_in_tx` and event-append errors.
    pub async fn fail_job(
        &self,
        id: JobId,
        reason: String,
        now: OffsetDateTime,
    ) -> Result<Job, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let job = self.jobs.fail_in_tx(&mut tx, id, now).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::JobFailed,
                    occurred_at: now,
                    subject_type: SubjectType::Job,
                    subject_id: Some(id.0),
                    trace_id: None,
                    payload: Event::JobFailed(JobFailedPayload {
                        job_id: id.0,
                        reason,
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(job)
    }

    /// Cancel a job and emit `job.cancelled` carrying `reason`.
    ///
    /// # Errors
    /// Propagates `JobRepo::cancel_in_tx` and event-append errors.
    pub async fn cancel_job(
        &self,
        id: JobId,
        reason: String,
        now: OffsetDateTime,
    ) -> Result<Job, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let job = self.jobs.cancel_in_tx(&mut tx, id, now).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::JobCancelled,
                    occurred_at: now,
                    subject_type: SubjectType::Job,
                    subject_id: Some(id.0),
                    trace_id: None,
                    payload: Event::JobCancelled(JobCancelledPayload {
                        job_id: id.0,
                        reason,
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(job)
    }
}

#[cfg(test)]
#[path = "jobs_test.rs"]
mod tests;
