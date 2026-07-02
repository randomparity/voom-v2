use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{FailureClass, IssueId, LeaseId, TicketId, VoomError};
use voom_store::repo::TerminalFailureIssueDraft;

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx, require_audit_field};

pub(crate) mod jobs;
pub(crate) mod leases;
pub(crate) mod remote_execution;
pub(crate) mod tickets;

impl ControlPlane {
    /// Open the one `terminal_failure` issue for a ticket's terminal
    /// transition inside the caller's transaction and return its id to stamp
    /// on the `TicketFailedTerminal` payload. Severity and priority derive
    /// from the failure taxonomy (`FailureClass::issue_severity` /
    /// `issue_priority`); `reason` becomes the issue body. `lease_id` is
    /// `None` on the pre-lease selection-failure path.
    ///
    /// # Errors
    /// Propagates `SqliteIssueRepo::open_terminal_failure_in_tx` errors.
    pub(crate) async fn open_terminal_failure_issue_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        ticket_id: TicketId,
        lease_id: Option<LeaseId>,
        class: FailureClass,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<IssueId, VoomError> {
        self.issues
            .open_terminal_failure_in_tx(
                tx,
                TerminalFailureIssueDraft {
                    ticket_id,
                    lease_id,
                    severity: class.issue_severity(),
                    priority: class.issue_priority(),
                    priority_reason: format!("terminal failure classified {class:?}"),
                    title: format!("Terminal failure on ticket {ticket_id}"),
                    body: reason.to_owned(),
                },
                now,
            )
            .await
    }
}
