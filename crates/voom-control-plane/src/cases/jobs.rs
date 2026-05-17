//! Job-lifecycle use cases. Each method opens a transaction, calls the
//! `JobRepo` `_in_tx` form, emits the matching event via
//! `EventRepo::append_in_tx`, then commits.

use time::OffsetDateTime;
use voom_core::{JobId, VoomError};
use voom_events::payload::{
    JobCancelledPayload, JobFailedPayload, JobOpenedPayload, JobSucceededPayload,
};
use voom_events::{Event, EventKind, SubjectType};
use voom_store::repo::jobs::{Job, JobRepo, NewJob};

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx};

impl ControlPlane {
    /// Open a new job and emit `job.opened` in the same transaction.
    ///
    /// # Errors
    /// Propagates `JobRepo::create_in_tx` and `EventRepo::append_in_tx` errors.
    pub async fn open_job(&self, input: NewJob) -> Result<Job, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let job = self.jobs.create_in_tx(&mut tx, input.clone()).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::JobOpened,
            SubjectType::Job,
            Some(job.id.0),
            input.created_at,
            Event::JobOpened(JobOpenedPayload {
                job_id: job.id.0,
                kind: input.kind.clone(),
                priority: input.priority,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(job)
    }

    /// Mark a job succeeded and emit `job.succeeded`.
    ///
    /// # Errors
    /// Propagates `JobRepo::succeed_in_tx` and event-append errors.
    pub async fn succeed_job(&self, id: JobId, now: OffsetDateTime) -> Result<Job, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let job = self.jobs.succeed_in_tx(&mut tx, id, now).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::JobSucceeded,
            SubjectType::Job,
            Some(id.0),
            now,
            Event::JobSucceeded(JobSucceededPayload { job_id: id.0 }),
        )
        .await?;
        commit_tx(tx).await?;
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
        let mut tx = begin_tx(&self.pool).await?;
        let job = self.jobs.fail_in_tx(&mut tx, id, now).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::JobFailed,
            SubjectType::Job,
            Some(id.0),
            now,
            Event::JobFailed(JobFailedPayload {
                job_id: id.0,
                reason,
            }),
        )
        .await?;
        commit_tx(tx).await?;
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
        let mut tx = begin_tx(&self.pool).await?;
        let job = self.jobs.cancel_in_tx(&mut tx, id, now).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::JobCancelled,
            SubjectType::Job,
            Some(id.0),
            now,
            Event::JobCancelled(JobCancelledPayload {
                job_id: id.0,
                reason,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(job)
    }
}

#[cfg(test)]
#[path = "jobs_test.rs"]
mod tests;
