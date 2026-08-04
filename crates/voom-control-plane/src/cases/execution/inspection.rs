use voom_core::{JobId, LeaseId, TicketId, VoomError};
use voom_events::EventId;
use voom_store::repo::{
    audit::events::{EventFilter, EventRepo, EventRow, Page},
    execution::{
        jobs::{Job, JobFilter},
        leases::{Lease, LeaseFilter},
        scheduler_decisions::{SchedulerDecision, SchedulerDecisionFilter},
        tickets::{Ticket, TicketFilter},
    },
};

use crate::ControlPlane;

impl ControlPlane {
    /// Read one durable scheduler decision.
    ///
    /// # Errors
    /// Propagates scheduler decision repository read errors.
    pub async fn scheduler_decision(
        &self,
        id: u64,
    ) -> Result<Option<SchedulerDecision>, VoomError> {
        self.scheduler_decisions.get(id).await
    }

    /// List durable scheduler decisions through a read-only `ControlPlane`
    /// surface. Scheduler decision writes remain owned by remote acquire.
    ///
    /// # Errors
    /// Propagates scheduler decision repository read errors.
    pub async fn scheduler_decisions(
        &self,
        filter: SchedulerDecisionFilter,
    ) -> Result<Vec<SchedulerDecision>, VoomError> {
        self.scheduler_decisions.list(filter).await
    }

    /// Keyset-paginated durable event inspection for `voom event list`
    /// (ADR 0031). Newest first; `after_id` continues with `id < after_id`.
    ///
    /// # Errors
    /// Propagates event repository read errors.
    pub async fn list_events(
        &self,
        filter: EventFilter,
        after_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<EventRow>, VoomError> {
        let page = Page {
            limit,
            cursor: after_id,
        };
        Ok(self.events.tail(filter, page).await?.items)
    }

    /// Read one durable event by id (`voom event show`).
    ///
    /// # Errors
    /// Propagates event repository read errors.
    pub async fn get_event(&self, id: u64) -> Result<Option<EventRow>, VoomError> {
        self.events.get(EventId(id)).await
    }

    /// Keyset-paginated durable job inspection for `voom job list` (ADR 0031).
    ///
    /// # Errors
    /// Propagates job repository read errors.
    pub async fn list_jobs(
        &self,
        filter: JobFilter,
        after_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<Job>, VoomError> {
        self.jobs.list(filter, after_id, limit).await
    }

    /// Read one durable job by id (`voom job show`).
    ///
    /// # Errors
    /// Propagates job repository read errors.
    pub async fn get_job(&self, id: u64) -> Result<Option<Job>, VoomError> {
        self.jobs.get(JobId(id)).await
    }

    /// Keyset-paginated durable ticket inspection for `voom ticket list`
    /// (ADR 0031).
    ///
    /// # Errors
    /// Propagates ticket repository read errors.
    pub async fn list_tickets(
        &self,
        filter: TicketFilter,
        after_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<Ticket>, VoomError> {
        self.tickets.list(filter, after_id, limit).await
    }

    /// Read one durable ticket by id (`voom ticket show`).
    ///
    /// # Errors
    /// Propagates ticket repository read errors.
    pub async fn get_ticket(&self, id: u64) -> Result<Option<Ticket>, VoomError> {
        self.tickets.get(TicketId(id)).await
    }

    /// Keyset-paginated durable scheduler-lease inspection for
    /// `voom scheduler leases list` (ADR 0031). These are the scheduler
    /// execution leases (`leases` table), distinct from operator use-leases.
    ///
    /// # Errors
    /// Propagates lease repository read errors.
    pub async fn list_scheduler_leases(
        &self,
        filter: LeaseFilter,
        after_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<Lease>, VoomError> {
        self.leases.list(filter, after_id, limit).await
    }

    /// Read one durable scheduler lease by id (`voom scheduler leases show`).
    ///
    /// # Errors
    /// Propagates lease repository read errors.
    pub async fn get_scheduler_lease(&self, id: u64) -> Result<Option<Lease>, VoomError> {
        self.leases.get(LeaseId(id)).await
    }
}

#[cfg(test)]
#[path = "inspection_test.rs"]
mod tests;
