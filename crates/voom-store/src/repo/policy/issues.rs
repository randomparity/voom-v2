use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{IssueId, IssuePriority, IssueSeverity, LeaseId, TicketId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, iso8601, u64_from_i64};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIssueDraft {
    pub dedupe_key: String,
    pub status: PolicyIssueStatus,
    pub title: String,
    pub body: String,
    pub priority_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyIssueStatus {
    Open,
    Planned,
    Resolved,
}

impl PolicyIssueStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Planned => "planned",
            Self::Resolved => "resolved",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "open" => Ok(Self::Open),
            "planned" => Ok(Self::Planned),
            "resolved" => Ok(Self::Resolved),
            other => Err(VoomError::database(format!(
                "issues.status {other:?} not in policy issue vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIssueRow {
    pub id: IssueId,
    pub dedupe_key: String,
    pub status: PolicyIssueStatus,
    pub epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyIssueMutationKind {
    Created,
    Updated,
    Resolved,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIssueMutation {
    pub kind: PolicyIssueMutationKind,
    pub row: PolicyIssueRow,
}

/// Everything needed to open the one `terminal_failure` issue for a ticket's
/// terminal transition. Severity and priority are supplied by the caller
/// (derived from the failure's `FailureClass` in the control plane) so the
/// store stays free of taxonomy knowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFailureIssueDraft {
    pub ticket_id: TicketId,
    /// Last lease held for the ticket, if any. The pre-lease selection
    /// failure path has no lease, so this is `None` there.
    pub lease_id: Option<LeaseId>,
    pub severity: IssueSeverity,
    pub priority: IssuePriority,
    pub priority_reason: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct SqliteIssueRepo;

impl SqliteIssueRepo {
    #[must_use]
    pub fn new(_pool: SqlitePool) -> Self {
        Self
    }
}

impl Repository for SqliteIssueRepo {}

impl SqliteIssueRepo {
    pub async fn upsert_policy_noncompliant_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        draft: PolicyIssueDraft,
        now: OffsetDateTime,
    ) -> Result<PolicyIssueMutation, VoomError> {
        let timestamp = iso8601(now)?;
        let inserted = sqlx::query(
            "INSERT INTO issues \
             (kind, severity, priority, priority_source, priority_reason, status, \
              suppressed_until, title, body, created_at, updated_at, resolved_at, dedupe_key) \
             VALUES ('policy_noncompliant', 'medium', 'normal', 'policy', ?, ?, \
                     NULL, ?, ?, ?, ?, NULL, ?)",
        )
        .bind(&draft.priority_reason)
        .bind(draft.status.as_str())
        .bind(&draft.title)
        .bind(&draft.body)
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(&draft.dedupe_key)
        .execute(&mut **tx)
        .await;

        let existing = match inserted {
            Ok(result) => {
                return Ok(PolicyIssueMutation {
                    kind: PolicyIssueMutationKind::Created,
                    row: PolicyIssueRow {
                        id: IssueId(u64_from_i64(result.last_insert_rowid())),
                        dedupe_key: draft.dedupe_key,
                        status: draft.status,
                        epoch: 0,
                    },
                });
            }
            Err(err) => {
                let existing = select_issue_detail(tx, &draft.dedupe_key).await?;
                let Some(existing) = existing else {
                    return Err(VoomError::database_context("issues insert", err));
                };
                existing
            }
        };
        if existing.row.status == draft.status
            && existing.title == draft.title
            && existing.body == draft.body
            && existing.priority_reason.as_deref() == Some(draft.priority_reason.as_str())
        {
            return Ok(PolicyIssueMutation {
                kind: PolicyIssueMutationKind::Unchanged,
                row: existing.row,
            });
        }

        sqlx::query(
            "UPDATE issues \
             SET status = ?, title = ?, body = ?, priority_reason = ?, updated_at = ?, \
                 resolved_at = NULL, epoch = epoch + 1 \
             WHERE id = ?",
        )
        .bind(draft.status.as_str())
        .bind(&draft.title)
        .bind(&draft.body)
        .bind(&draft.priority_reason)
        .bind(&timestamp)
        .bind(i64_from_u64(existing.row.id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("issues update", e))?;

        Ok(PolicyIssueMutation {
            kind: PolicyIssueMutationKind::Updated,
            row: PolicyIssueRow {
                id: existing.row.id,
                dedupe_key: existing.row.dedupe_key,
                status: draft.status,
                epoch: existing.row.epoch + 1,
            },
        })
    }

    pub async fn resolve_policy_noncompliant_by_dedupe_key_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        dedupe_key: &str,
        title: &str,
        body: &str,
        now: OffsetDateTime,
    ) -> Result<Option<PolicyIssueMutation>, VoomError> {
        let Some(existing) = select_live_policy_issue(tx, dedupe_key).await? else {
            return Ok(None);
        };
        let timestamp = iso8601(now)?;
        sqlx::query(
            "UPDATE issues \
             SET status = 'resolved', title = ?, body = ?, updated_at = ?, resolved_at = ?, \
                 epoch = epoch + 1 \
             WHERE id = ?",
        )
        .bind(title)
        .bind(body)
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(i64_from_u64(existing.id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("issues resolve", e))?;

        Ok(Some(PolicyIssueMutation {
            kind: PolicyIssueMutationKind::Resolved,
            row: PolicyIssueRow {
                status: PolicyIssueStatus::Resolved,
                epoch: existing.epoch + 1,
                ..existing
            },
        }))
    }

    pub async fn list_live_policy_noncompliant_by_dedupe_prefix_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        dedupe_prefix: &str,
    ) -> Result<Vec<PolicyIssueRow>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, dedupe_key, status, epoch \
             FROM issues \
             WHERE kind = 'policy_noncompliant' \
               AND status IN ('open', 'planned') \
               AND dedupe_key LIKE ? ESCAPE '\\' \
             ORDER BY id ASC",
        )
        .bind(dedupe_prefix)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("issues list live", e))?;

        rows.iter().map(row_to_policy_issue).collect()
    }

    /// Open the single `terminal_failure` issue for a ticket's terminal
    /// transition and return its id, inserting `issue_links` for the ticket
    /// and (when present) its last lease. Idempotent: the dedupe key is
    /// derived solely from the ticket id, so a re-run of the same terminal
    /// transaction returns the existing issue id without inserting a
    /// duplicate row or duplicate links. A ticket reaches `failed` at most
    /// once, so under normal operation exactly one issue is opened per
    /// terminal transition (spec: Issue Model + Failure taxonomy).
    ///
    /// # Errors
    /// Propagates database errors from the issue insert, the dedupe lookup on
    /// conflict, and the `issue_links` inserts.
    pub async fn open_terminal_failure_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        draft: TerminalFailureIssueDraft,
        now: OffsetDateTime,
    ) -> Result<IssueId, VoomError> {
        let dedupe_key = terminal_failure_dedupe_key(draft.ticket_id);
        let timestamp = iso8601(now)?;
        let inserted = sqlx::query(
            "INSERT INTO issues \
             (kind, severity, priority, priority_source, priority_reason, status, \
              suppressed_until, title, body, created_at, updated_at, resolved_at, dedupe_key) \
             VALUES ('terminal_failure', ?, ?, 'system', ?, 'open', \
                     NULL, ?, ?, ?, ?, NULL, ?)",
        )
        .bind(draft.severity.as_str())
        .bind(draft.priority.as_str())
        .bind(&draft.priority_reason)
        .bind(&draft.title)
        .bind(&draft.body)
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(&dedupe_key)
        .execute(&mut **tx)
        .await;

        let issue_id = match inserted {
            Ok(result) => IssueId(u64_from_i64(result.last_insert_rowid())),
            Err(err) => {
                let Some(existing) = select_terminal_failure_id(tx, &dedupe_key).await? else {
                    return Err(VoomError::database_context(
                        "terminal_failure issue insert",
                        err,
                    ));
                };
                return Ok(existing);
            }
        };

        insert_issue_link(tx, issue_id, "ticket", draft.ticket_id.0, &timestamp).await?;
        if let Some(lease_id) = draft.lease_id {
            insert_issue_link(tx, issue_id, "lease", lease_id.0, &timestamp).await?;
        }
        Ok(issue_id)
    }
}

fn terminal_failure_dedupe_key(ticket_id: TicketId) -> String {
    format!("terminal_failure:ticket:{}", ticket_id.0)
}

async fn select_terminal_failure_id(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    dedupe_key: &str,
) -> Result<Option<IssueId>, VoomError> {
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM issues WHERE kind = 'terminal_failure' AND dedupe_key = ?",
    )
    .bind(dedupe_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("terminal_failure issue select", e))?;
    Ok(id.map(|id| IssueId(u64_from_i64(id))))
}

async fn insert_issue_link(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    issue_id: IssueId,
    link_kind: &str,
    target_id: u64,
    timestamp: &str,
) -> Result<(), VoomError> {
    sqlx::query(
        "INSERT INTO issue_links (issue_id, link_type, target_type, target_id, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(i64_from_u64(issue_id.0))
    .bind(link_kind)
    .bind(link_kind)
    .bind(i64_from_u64(target_id))
    .bind(timestamp)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("issue_link insert", e))?;
    Ok(())
}

#[derive(Debug)]
struct PolicyIssueDetail {
    row: PolicyIssueRow,
    title: String,
    body: String,
    priority_reason: Option<String>,
}

async fn select_issue_detail(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    dedupe_key: &str,
) -> Result<Option<PolicyIssueDetail>, VoomError> {
    let row = sqlx::query(
        "SELECT id, dedupe_key, status, epoch, title, body, priority_reason \
         FROM issues WHERE kind = 'policy_noncompliant' AND dedupe_key = ?",
    )
    .bind(dedupe_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("issues select detail", e))?;
    row.as_ref().map(row_to_policy_issue_detail).transpose()
}

async fn select_live_policy_issue(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    dedupe_key: &str,
) -> Result<Option<PolicyIssueRow>, VoomError> {
    let row = sqlx::query(
        "SELECT id, dedupe_key, status, epoch \
         FROM issues \
         WHERE kind = 'policy_noncompliant' \
           AND dedupe_key = ? \
           AND status IN ('open', 'planned')",
    )
    .bind(dedupe_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("issues select live", e))?;
    row.as_ref().map(row_to_policy_issue).transpose()
}

fn row_to_policy_issue(row: &sqlx::sqlite::SqliteRow) -> Result<PolicyIssueRow, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| VoomError::database_context("read issue id", e))?;
    let dedupe_key: String = row
        .try_get("dedupe_key")
        .map_err(|e| VoomError::database_context("read issue dedupe_key", e))?;
    let status: String = row
        .try_get("status")
        .map_err(|e| VoomError::database_context("read issue status", e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| VoomError::database_context("read issue epoch", e))?;
    Ok(PolicyIssueRow {
        id: IssueId(u64_from_i64(id)),
        dedupe_key,
        status: PolicyIssueStatus::parse(&status)?,
        epoch: u64_from_i64(epoch),
    })
}

fn row_to_policy_issue_detail(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PolicyIssueDetail, VoomError> {
    let detail = PolicyIssueDetail {
        row: row_to_policy_issue(row)?,
        title: row
            .try_get("title")
            .map_err(|e| VoomError::database_context("read issue title", e))?,
        body: row
            .try_get("body")
            .map_err(|e| VoomError::database_context("read issue body", e))?,
        priority_reason: row
            .try_get("priority_reason")
            .map_err(|e| VoomError::database_context("read issue priority_reason", e))?,
    };
    Ok(detail)
}

#[cfg(test)]
#[path = "issues_test.rs"]
mod tests;
