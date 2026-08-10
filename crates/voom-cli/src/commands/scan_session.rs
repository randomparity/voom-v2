use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_control_plane::scan::{
    ScanReconciliationEvidence, ScanReconciliationQuery, ScanSession, ScanSessionListQuery,
};
use voom_core::{
    FileLocationId, ScanSessionId, ScanSessionStatus, ScanTerminalReason, StorageRootId,
    format_iso8601,
};

use crate::cli::{ScanSessionCommand, ScanSessionStatusArg};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_ok, emit_ok_page};

const COMMAND: &str = "scan-session";

#[derive(Debug, Serialize)]
struct SessionEnvelopeData {
    session: SessionData,
}

#[derive(Debug, Serialize)]
struct SessionListData {
    sessions: Vec<SessionData>,
}

#[derive(Debug, Serialize)]
struct ReconciliationPageData {
    items: Vec<ReconciliationData>,
}

#[derive(Debug, Serialize)]
struct SessionData {
    id: u64,
    storage_root_id: u64,
    root_epoch: u64,
    owner_node_id: u64,
    owner_incarnation_id: Option<String>,
    status: &'static str,
    next_sequence: u64,
    batch_count: u64,
    observation_count: u64,
    idle_timeout_seconds: u32,
    progress_deadline_at: String,
    location_high_watermark_id: Option<u64>,
    requested_at: String,
    started_at: Option<String>,
    terminal_at: Option<String>,
    terminal_reason: Option<String>,
    retired_location_count: u64,
    reconciliation_applied: bool,
}

impl From<ScanSession> for SessionData {
    fn from(session: ScanSession) -> Self {
        Self {
            id: session.id.0,
            storage_root_id: session.storage_root_id.0,
            root_epoch: session.root_epoch,
            owner_node_id: session.owner_node_id.0,
            owner_incarnation_id: session.owner_incarnation_id.map(|id| id.to_string()),
            status: session.status.as_str(),
            next_sequence: session.next_sequence,
            batch_count: session.batch_count,
            observation_count: session.observation_count,
            idle_timeout_seconds: session.idle_timeout_seconds,
            progress_deadline_at: format_iso8601(session.progress_deadline_at),
            location_high_watermark_id: session.location_high_watermark_id.map(|id| id.0),
            requested_at: format_iso8601(session.requested_at),
            started_at: session.started_at.map(format_iso8601),
            terminal_at: session.terminal_at.map(format_iso8601),
            terminal_reason: session
                .terminal_reason
                .map(|reason| reason.as_str().to_owned()),
            retired_location_count: session.retired_location_count,
            reconciliation_applied: session.status == ScanSessionStatus::Succeeded,
        }
    }
}

#[derive(Debug, Serialize)]
struct ReconciliationData {
    file_location_id: u64,
    retired_at: String,
    prior_epoch: u64,
    retired_epoch: u64,
}

impl From<ScanReconciliationEvidence> for ReconciliationData {
    fn from(evidence: ScanReconciliationEvidence) -> Self {
        Self {
            file_location_id: evidence.file_location_id.0,
            retired_at: format_iso8601(evidence.retired_at),
            prior_epoch: evidence.prior_epoch,
            retired_epoch: evidence.retired_epoch,
        }
    }
}

pub async fn run(database_url: &str, local: Local, command: ScanSessionCommand) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        ScanSessionCommand::Request {
            root,
            idle_timeout_seconds,
        } => request(&cp, root, idle_timeout_seconds, local).await,
        ScanSessionCommand::Show { id } => show(&cp, id, local).await,
        ScanSessionCommand::List {
            root,
            status,
            after,
            limit,
        } => {
            list(
                &cp,
                root,
                status.map(ScanSessionStatusArg::to_core),
                after,
                limit,
                local,
            )
            .await
        }
        ScanSessionCommand::Reconciliation { id, after, limit } => {
            reconciliation(&cp, id, after, limit, local).await
        }
        ScanSessionCommand::Cancel { id, reason } => cancel(&cp, id, reason, local).await,
    }
}

async fn request(
    cp: &ControlPlane,
    root: u64,
    idle_timeout_seconds: u32,
    local: Local,
) -> io::Result<i32> {
    emit_session(
        cp.request_scan_session(StorageRootId(root), idle_timeout_seconds)
            .await,
        local,
    )
}

async fn show(cp: &ControlPlane, id: u64, local: Local) -> io::Result<i32> {
    emit_session(cp.scan_session(ScanSessionId(id)).await, local)
}

async fn list(
    cp: &ControlPlane,
    root: Option<u64>,
    status: Option<ScanSessionStatus>,
    after: Option<u64>,
    limit: u32,
    local: Local,
) -> io::Result<i32> {
    let query = ScanSessionListQuery {
        storage_root_id: root.map(StorageRootId),
        status,
        after_id: after.map(ScanSessionId),
        limit,
    };
    match cp.scan_sessions(query).await {
        Ok(page) => emit_ok_page(
            COMMAND,
            SessionListData {
                sessions: page.items.into_iter().map(SessionData::from).collect(),
            },
            page.next_after_id.map(|id| id.0),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(error) => emit_voom_error(COMMAND, &error, local),
    }
}

async fn reconciliation(
    cp: &ControlPlane,
    id: u64,
    after: Option<u64>,
    limit: u32,
    local: Local,
) -> io::Result<i32> {
    let query = ScanReconciliationQuery {
        scan_session_id: ScanSessionId(id),
        after_id: after.map(FileLocationId),
        limit,
    };
    match cp.scan_reconciliation(query).await {
        Ok(page) => emit_ok_page(
            COMMAND,
            ReconciliationPageData {
                items: page
                    .items
                    .into_iter()
                    .map(ReconciliationData::from)
                    .collect(),
            },
            page.next_after_id.map(|id| id.0),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(error) => emit_voom_error(COMMAND, &error, local),
    }
}

async fn cancel(cp: &ControlPlane, id: u64, reason: String, local: Local) -> io::Result<i32> {
    let reason = match ScanTerminalReason::new(reason) {
        Ok(reason) => reason,
        Err(error) => return emit_voom_error(COMMAND, &error, local),
    };
    emit_session(
        cp.cancel_scan_session(ScanSessionId(id), reason).await,
        local,
    )
}

fn emit_session(
    result: Result<ScanSession, voom_core::VoomError>,
    local: Local,
) -> io::Result<i32> {
    match result {
        Ok(session) => emit_ok(
            COMMAND,
            SessionEnvelopeData {
                session: SessionData::from(session),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(error) => emit_voom_error(COMMAND, &error, local),
    }
}

#[cfg(test)]
#[path = "scan_session_test.rs"]
mod tests;
