use std::fmt::Write as _;

use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{IssueId, IssuePriority, IssueSeverity, LeaseId, TicketId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, iso8601, parse_iso8601, u64_from_i64};

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
pub struct SqliteIssueRepo {
    pool: SqlitePool,
}

impl SqliteIssueRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
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

// ============================================================================
// Generalized issue read + transition surface (T14, #283)
//
// The write paths above are kind-specific (`policy_noncompliant` upsert,
// `terminal_failure` open). The operator CLI (`voom issue …`) reads and
// transitions issues of *any* kind, so these types project the full `issues`
// row and expose the complete status vocabulary the table's CHECK allows.
// ============================================================================

/// The full `issues.status` vocabulary. `PolicyIssueStatus` above is the
/// subset the policy-compliance write path emits; the operator surface can
/// observe and set every state the schema permits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueStatus {
    Open,
    Planned,
    Resolved,
    Suppressed,
    Accepted,
}

impl IssueStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Planned => "planned",
            Self::Resolved => "resolved",
            Self::Suppressed => "suppressed",
            Self::Accepted => "accepted",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "open" => Ok(Self::Open),
            "planned" => Ok(Self::Planned),
            "resolved" => Ok(Self::Resolved),
            "suppressed" => Ok(Self::Suppressed),
            "accepted" => Ok(Self::Accepted),
            other => Err(VoomError::database(format!(
                "issues.status {other:?} not in issue status vocab"
            ))),
        }
    }
}

/// A full `issues` row projected for the operator read surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRecord {
    pub id: IssueId,
    pub kind: String,
    pub severity: IssueSeverity,
    pub priority: IssuePriority,
    pub priority_source: String,
    pub priority_reason: Option<String>,
    pub status: IssueStatus,
    pub suppressed_until: Option<OffsetDateTime>,
    pub title: String,
    pub body: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub resolved_at: Option<OffsetDateTime>,
    pub epoch: u64,
    pub dedupe_key: Option<String>,
}

/// Optional equality filters for `list_issues`. A `None` field is unconstrained.
#[derive(Debug, Clone, Default)]
pub struct IssueFilter {
    pub status: Option<IssueStatus>,
    pub kind: Option<String>,
    pub priority: Option<IssuePriority>,
    pub severity: Option<IssueSeverity>,
}

/// One keyset page of issues plus the cursor to resume after it. `next_cursor`
/// is the id of the last row returned (`None` only for an empty page), matching
/// the forward-list convention of `EventRepo::list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueListPage {
    pub items: Vec<IssueRecord>,
    pub next_cursor: Option<u64>,
}

const ISSUE_COLUMNS: &str = "id, kind, severity, priority, priority_source, priority_reason, \
     status, suppressed_until, title, body, created_at, updated_at, resolved_at, epoch, dedupe_key";

impl SqliteIssueRepo {
    /// List issues ordered by ascending id, filtered and keyset-paginated.
    /// Rows with `id > cursor` are returned, up to `limit`.
    ///
    /// # Errors
    /// Propagates database errors and fails if a stored enum column value is
    /// outside its vocabulary (data corruption).
    pub async fn list_issues(
        &self,
        filter: &IssueFilter,
        cursor: Option<u64>,
        limit: u32,
    ) -> Result<IssueListPage, VoomError> {
        let mut sql = format!("SELECT {ISSUE_COLUMNS} FROM issues WHERE 1=1");
        if filter.status.is_some() {
            sql.push_str(" AND status = ?");
        }
        if filter.kind.is_some() {
            sql.push_str(" AND kind = ?");
        }
        if filter.priority.is_some() {
            sql.push_str(" AND priority = ?");
        }
        if filter.severity.is_some() {
            sql.push_str(" AND severity = ?");
        }
        if cursor.is_some() {
            sql.push_str(" AND id > ?");
        }
        write!(sql, " ORDER BY id ASC LIMIT ?")
            .map_err(|e| VoomError::Internal(format!("build issue list SQL: {e}")))?;

        let mut q = sqlx::query(&sql);
        if let Some(status) = filter.status {
            q = q.bind(status.as_str());
        }
        if let Some(kind) = &filter.kind {
            q = q.bind(kind.as_str());
        }
        if let Some(priority) = filter.priority {
            q = q.bind(priority.as_str());
        }
        if let Some(severity) = filter.severity {
            q = q.bind(severity.as_str());
        }
        if let Some(cursor) = cursor {
            q = q.bind(i64_from_u64(cursor));
        }
        q = q.bind(i64::from(limit));

        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("issues list", e))?;
        let items = rows
            .iter()
            .map(row_to_issue_record)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = items.last().map(|record| record.id.0);
        Ok(IssueListPage { items, next_cursor })
    }

    /// Read one issue by id.
    ///
    /// # Errors
    /// Propagates database errors and enum-vocabulary violations.
    pub async fn get_issue(&self, id: IssueId) -> Result<Option<IssueRecord>, VoomError> {
        let row = sqlx::query(&format!("SELECT {ISSUE_COLUMNS} FROM issues WHERE id = ?"))
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("issues get", e))?;
        row.as_ref().map(row_to_issue_record).transpose()
    }

    /// Override an issue's priority (and, when supplied, its reason), stamping
    /// `priority_source = 'user'`. Status is untouched, so the resolved/
    /// suppressed timestamp invariants are preserved. `Ok(None)` when no issue
    /// has that id.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn update_priority_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: IssueId,
        priority: IssuePriority,
        priority_reason: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<Option<IssueRecord>, VoomError> {
        let timestamp = iso8601(now)?;
        let result = match priority_reason {
            Some(reason) => sqlx::query(
                "UPDATE issues \
                 SET priority = ?, priority_source = 'user', priority_reason = ?, \
                     updated_at = ?, epoch = epoch + 1 \
                 WHERE id = ?",
            )
            .bind(priority.as_str())
            .bind(reason)
            .bind(&timestamp)
            .bind(i64_from_u64(id.0)),
            None => sqlx::query(
                "UPDATE issues \
                 SET priority = ?, priority_source = 'user', updated_at = ?, epoch = epoch + 1 \
                 WHERE id = ?",
            )
            .bind(priority.as_str())
            .bind(&timestamp)
            .bind(i64_from_u64(id.0)),
        }
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("issues update priority", e))?;
        self.record_after_transition(tx, id, result.rows_affected())
            .await
    }

    /// Transition an issue to `resolved`, stamping `resolved_at` and clearing
    /// any `suppressed_until` horizon. `Ok(None)` when no issue has that id.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn resolve_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: IssueId,
        now: OffsetDateTime,
    ) -> Result<Option<IssueRecord>, VoomError> {
        let timestamp = iso8601(now)?;
        let result = sqlx::query(
            "UPDATE issues \
             SET status = 'resolved', resolved_at = ?, suppressed_until = NULL, \
                 updated_at = ?, epoch = epoch + 1 \
             WHERE id = ?",
        )
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("issues resolve", e))?;
        self.record_after_transition(tx, id, result.rows_affected())
            .await
    }

    /// Transition an issue to `suppressed` until `until`, clearing any
    /// `resolved_at`. `Ok(None)` when no issue has that id.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn suppress_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: IssueId,
        until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<Option<IssueRecord>, VoomError> {
        let timestamp = iso8601(now)?;
        let until_ts = iso8601(until)?;
        let result = sqlx::query(
            "UPDATE issues \
             SET status = 'suppressed', suppressed_until = ?, resolved_at = NULL, \
                 updated_at = ?, epoch = epoch + 1 \
             WHERE id = ?",
        )
        .bind(&until_ts)
        .bind(&timestamp)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("issues suppress", e))?;
        self.record_after_transition(tx, id, result.rows_affected())
            .await
    }

    /// Transition an issue to `accepted`, clearing any resolved/suppressed
    /// bookkeeping. `Ok(None)` when no issue has that id.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn accept_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: IssueId,
        now: OffsetDateTime,
    ) -> Result<Option<IssueRecord>, VoomError> {
        let timestamp = iso8601(now)?;
        let result = sqlx::query(
            "UPDATE issues \
             SET status = 'accepted', resolved_at = NULL, suppressed_until = NULL, \
                 updated_at = ?, epoch = epoch + 1 \
             WHERE id = ?",
        )
        .bind(&timestamp)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("issues accept", e))?;
        self.record_after_transition(tx, id, result.rows_affected())
            .await
    }

    /// Re-read the row an in-tx transition just wrote, or `Ok(None)` when the
    /// UPDATE matched no row (unknown id).
    async fn record_after_transition(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: IssueId,
        rows_affected: u64,
    ) -> Result<Option<IssueRecord>, VoomError> {
        if rows_affected == 0 {
            return Ok(None);
        }
        select_issue_record_in_tx(tx, id).await
    }
}

async fn select_issue_record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: IssueId,
) -> Result<Option<IssueRecord>, VoomError> {
    let row = sqlx::query(&format!("SELECT {ISSUE_COLUMNS} FROM issues WHERE id = ?"))
        .bind(i64_from_u64(id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("issues select record", e))?;
    row.as_ref().map(row_to_issue_record).transpose()
}

fn row_to_issue_record(row: &sqlx::sqlite::SqliteRow) -> Result<IssueRecord, VoomError> {
    let id: i64 = try_get(row, "id")?;
    let severity: String = try_get(row, "severity")?;
    let priority: String = try_get(row, "priority")?;
    let status: String = try_get(row, "status")?;
    let epoch: i64 = try_get(row, "epoch")?;
    let suppressed_until: Option<String> = try_get(row, "suppressed_until")?;
    let created_at: String = try_get(row, "created_at")?;
    let updated_at: String = try_get(row, "updated_at")?;
    let resolved_at: Option<String> = try_get(row, "resolved_at")?;
    Ok(IssueRecord {
        id: IssueId(u64_from_i64(id)),
        kind: try_get(row, "kind")?,
        severity: IssueSeverity::parse(&severity)?,
        priority: IssuePriority::parse(&priority)?,
        priority_source: try_get(row, "priority_source")?,
        priority_reason: try_get(row, "priority_reason")?,
        status: IssueStatus::parse(&status)?,
        suppressed_until: suppressed_until.as_deref().map(parse_iso8601).transpose()?,
        title: try_get(row, "title")?,
        body: try_get(row, "body")?,
        created_at: parse_iso8601(&created_at)?,
        updated_at: parse_iso8601(&updated_at)?,
        resolved_at: resolved_at.as_deref().map(parse_iso8601).transpose()?,
        epoch: u64_from_i64(epoch),
        dedupe_key: try_get(row, "dedupe_key")?,
    })
}

fn try_get<'r, T>(row: &'r sqlx::sqlite::SqliteRow, column: &str) -> Result<T, VoomError>
where
    T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get(column)
        .map_err(|e| VoomError::database_context(format!("read issue {column}"), e))
}

#[cfg(test)]
#[path = "issues_test.rs"]
mod tests;
