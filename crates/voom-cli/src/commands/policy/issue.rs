use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_core::IssueId;
use voom_store::repo::issues::{IssueFilter, IssueRecord};

use crate::cli::{IssueCommand, IssuePriorityArg, IssueSeverityArg, IssueStatusArg};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

const COMMAND: &str = "issue";

#[derive(Debug, Serialize)]
pub struct IssueWire {
    pub id: u64,
    pub kind: String,
    pub severity: String,
    pub priority: String,
    pub priority_source: String,
    pub priority_reason: Option<String>,
    pub status: String,
    pub suppressed_until: Option<String>,
    pub title: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    pub resolved_at: Option<String>,
    pub epoch: u64,
    pub dedupe_key: Option<String>,
}

impl From<IssueRecord> for IssueWire {
    fn from(record: IssueRecord) -> Self {
        Self {
            id: record.id.0,
            kind: record.kind,
            severity: record.severity.as_str().to_owned(),
            priority: record.priority.as_str().to_owned(),
            priority_source: record.priority_source,
            priority_reason: record.priority_reason,
            status: record.status.as_str().to_owned(),
            suppressed_until: record.suppressed_until.map(voom_core::format_iso8601),
            title: record.title,
            body: record.body,
            created_at: voom_core::format_iso8601(record.created_at),
            updated_at: voom_core::format_iso8601(record.updated_at),
            resolved_at: record.resolved_at.map(voom_core::format_iso8601),
            epoch: record.epoch,
            dedupe_key: record.dedupe_key,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct IssueListData {
    pub issues: Vec<IssueWire>,
    pub next_cursor: Option<u64>,
}

pub async fn run(database_url: &str, local: Local, command: IssueCommand) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        IssueCommand::List {
            status,
            kind,
            priority,
            severity,
            after_id,
            limit,
        } => {
            let filter = IssueFilter {
                status: status.map(IssueStatusArg::to_store),
                kind,
                priority: priority.map(IssuePriorityArg::to_core),
                severity: severity.map(IssueSeverityArg::to_core),
            };
            list(&cp, filter, after_id, limit, local).await
        }
        IssueCommand::Show { issue_id } => {
            emit_optional(cp.get_issue(IssueId(issue_id)).await, issue_id, local)
        }
        IssueCommand::Update {
            issue_id,
            priority,
            priority_reason,
        } => emit_optional(
            cp.update_issue_priority(IssueId(issue_id), priority.to_core(), priority_reason)
                .await,
            issue_id,
            local,
        ),
        IssueCommand::Resolve { issue_id } => {
            emit_optional(cp.resolve_issue(IssueId(issue_id)).await, issue_id, local)
        }
        IssueCommand::Suppress { issue_id, days } => emit_optional(
            cp.suppress_issue(IssueId(issue_id), days).await,
            issue_id,
            local,
        ),
        IssueCommand::Accept { issue_id } => {
            emit_optional(cp.accept_issue(IssueId(issue_id)).await, issue_id, local)
        }
    }
}

async fn list(
    cp: &ControlPlane,
    filter: IssueFilter,
    after_id: Option<u64>,
    limit: u32,
    local: Local,
) -> io::Result<i32> {
    match cp.list_issues(&filter, after_id, limit).await {
        Ok(page) => emit_ok(
            COMMAND,
            IssueListData {
                issues: page.items.into_iter().map(IssueWire::from).collect(),
                next_cursor: page.next_cursor,
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_optional(
    result: Result<Option<IssueRecord>, voom_core::VoomError>,
    issue_id: u64,
    local: Local,
) -> io::Result<i32> {
    match result {
        Ok(Some(record)) => {
            emit_ok(COMMAND, IssueWire::from(record), Some(local), Vec::new()).map(|()| 0)
        }
        Ok(None) => not_found(issue_id, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn not_found(issue_id: u64, local: Local) -> io::Result<i32> {
    emit_err(
        COMMAND,
        voom_core::ErrorCode::NotFound.as_str(),
        format!("issue {issue_id} not found"),
        None,
        Some(local),
    )?;
    Ok(2)
}
