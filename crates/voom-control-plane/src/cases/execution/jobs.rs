//! Job-lifecycle use cases. Each method opens a transaction, calls the
//! `SqliteJobRepo` `_in_tx` form, emits the matching event via
//! `EventRepo::append_in_tx`, then commits.

use time::OffsetDateTime;
use voom_core::{JobId, VoomError};
use voom_events::payload::{
    JobCancelledPayload, JobFailedPayload, JobOpenedPayload, JobSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::execution::jobs::{Job, NewJob};

use crate::ControlPlane;

use super::{append_event, commit_tx, require_audit_field};
use voom_store::tx::begin_write_first;

impl ControlPlane {
    /// Open a job and append its audit event in the caller's transaction.
    ///
    /// # Errors
    /// Propagates job insertion and event append failures.
    pub(crate) async fn open_job_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewJob,
    ) -> Result<Job, VoomError> {
        let job = self.jobs.create_in_tx(tx, input.clone()).await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Job,
            Some(job.id.0),
            input.created_at,
            Event::JobOpened(JobOpenedPayload {
                job_id: job.id,
                kind: input.kind,
                priority: input.priority,
            }),
        )
        .await?;
        Ok(job)
    }

    /// Open a new job and emit `job.opened` in the same transaction.
    ///
    /// # Errors
    /// Propagates `SqliteJobRepo::create_in_tx` and `EventRepo::append_in_tx` errors.
    pub async fn open_job(&self, input: NewJob) -> Result<Job, VoomError> {
        let mut tx = begin_write_first(&self.pool, "jobs: open_job").await?;
        let job = self.open_job_in_tx(&mut tx, input).await?;
        commit_tx(tx).await?;
        Ok(job)
    }

    /// Mark a job succeeded and emit `job.succeeded`.
    ///
    /// # Errors
    /// Propagates `SqliteJobRepo::succeed_in_tx` and event-append errors.
    pub async fn succeed_job(&self, id: JobId, now: OffsetDateTime) -> Result<Job, VoomError> {
        let mut tx = begin_write_first(&self.pool, "jobs: succeed_job").await?;
        let job = self.jobs.succeed_in_tx(&mut tx, id, now).await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Job,
            Some(id.0),
            now,
            Event::JobSucceeded(JobSucceededPayload { job_id: id }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(job)
    }

    /// Mark a job failed and emit `job.failed` carrying `reason`.
    ///
    /// # Errors
    /// Returns [`VoomError::Config`] when `reason` is blank. Propagates
    /// `SqliteJobRepo::fail_in_tx` and event-append errors.
    pub async fn fail_job(
        &self,
        id: JobId,
        reason: String,
        now: OffsetDateTime,
    ) -> Result<Job, VoomError> {
        require_audit_field("reason", &reason)?;
        let mut tx = begin_write_first(&self.pool, "jobs: fail_job").await?;
        let job = self.jobs.fail_in_tx(&mut tx, id, now).await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Job,
            Some(id.0),
            now,
            Event::JobFailed(JobFailedPayload { job_id: id, reason }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(job)
    }

    /// Cancel a job and emit `job.cancelled` carrying `reason`.
    ///
    /// # Errors
    /// Propagates `SqliteJobRepo::cancel_in_tx` and event-append errors.
    pub async fn cancel_job(
        &self,
        id: JobId,
        reason: String,
        now: OffsetDateTime,
    ) -> Result<Job, VoomError> {
        require_audit_field("reason", &reason)?;
        let mut tx = begin_write_first(&self.pool, "jobs: cancel_job").await?;
        let job = self.jobs.cancel_in_tx(&mut tx, id, now).await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Job,
            Some(id.0),
            now,
            Event::JobCancelled(JobCancelledPayload { job_id: id, reason }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(job)
    }
}

#[cfg(test)]
#[path = "jobs_test.rs"]
mod tests;
