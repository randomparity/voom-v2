//! `voom scan --root <id>`: request a durable scan run and, unless
//! `--no-wait`, poll it to a terminal state. The bytes are read by the owner
//! node's workers (ADR 0077); the CLI only talks to the control plane.

use std::io;
use std::time::Duration;

use serde::Serialize;
use voom_control_plane::scan::{RootScanBlocked, ScanRunOutcome};
use voom_core::{ErrorCode, StorageRootId};

use crate::commands::common::open_control_plane;
use crate::envelope::{Local, emit_err_with_data_and_warnings, emit_ok};

/// Idle timeout granted to the requested session; the agent-side pump must
/// make progress well inside it.
const DEFAULT_IDLE_TIMEOUT_SECONDS: u32 = 600;
/// How often the CLI re-reads the session while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The request outcome: the durable session and its ready ticket.
#[derive(Debug, Serialize)]
pub struct ScanRequestData {
    pub scan_session_id: u64,
    pub ticket_id: u64,
}

/// The waited outcome: the session's terminal state and publication counters.
#[derive(Debug, Serialize)]
pub struct ScanOutcomeData {
    pub scan_session_id: u64,
    pub status: String,
    pub observation_count: u64,
    pub retired_location_count: u64,
}

/// A blocked root: nothing was requested.
#[derive(Debug, Serialize)]
pub struct BlockedData {
    pub status: &'static str,
    pub reason: &'static str,
    pub library_id: u64,
    pub storage_root_id: u64,
    pub provider_locator: String,
}

impl From<RootScanBlocked> for BlockedData {
    fn from(blocked: RootScanBlocked) -> Self {
        Self {
            status: "blocked",
            reason: blocked.reason.as_str(),
            library_id: blocked.library_id.0,
            storage_root_id: blocked.storage_root_id.0,
            provider_locator: blocked.provider_locator,
        }
    }
}

pub async fn run(database_url: &str, local: Local, root: u64, no_wait: bool) -> io::Result<i32> {
    run_root(database_url, local, StorageRootId(root), no_wait).await
}

async fn run_root(
    database_url: &str,
    local: Local,
    root_id: StorageRootId,
    no_wait: bool,
) -> io::Result<i32> {
    let cp = match open_control_plane("scan", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .request_scan_run(root_id, DEFAULT_IDLE_TIMEOUT_SECONDS)
        .await
    {
        Ok(ScanRunOutcome::Requested(requested)) => {
            if no_wait {
                return emit_ok(
                    "scan",
                    ScanRequestData {
                        scan_session_id: requested.scan_session_id.0,
                        ticket_id: requested.ticket_id.0,
                    },
                    Some(local),
                    Vec::new(),
                )
                .map(|()| 0);
            }
            wait_for_terminal(&cp, local, requested.scan_session_id.0).await
        }
        Ok(ScanRunOutcome::Blocked(blocked)) => {
            let message = format!(
                "library root {} is blocked ({}); scan not requested",
                blocked.storage_root_id,
                blocked.reason.as_str()
            );
            emit_err_with_data_and_warnings(
                "scan",
                BlockedData::from(blocked),
                ErrorCode::Blocked.as_str(),
                message,
                None,
                Some(local),
                Vec::new(),
            )?;
            Ok(2)
        }
        Err(err) => {
            let message = err.to_string();
            emit_err_with_data_and_warnings(
                "scan",
                serde_json::json!({ "storage_root_id": root_id.0 }),
                err.code(),
                message,
                None,
                Some(local),
                Vec::new(),
            )?;
            Ok(2)
        }
    }
}

/// Poll the session until it reaches a terminal state, bounded by the granted
/// idle timeout plus a publication grace window.
async fn wait_for_terminal(
    cp: &voom_control_plane::ControlPlane,
    local: Local,
    scan_session_id: u64,
) -> io::Result<i32> {
    let session_id = voom_core::ScanSessionId(scan_session_id);
    let deadline = std::time::Instant::now()
        + Duration::from_secs(u64::from(DEFAULT_IDLE_TIMEOUT_SECONDS) + 60);
    loop {
        match cp.scan_session(session_id).await {
            Ok(session) => {
                if is_terminal(session.status) {
                    let status = session.status.as_str().to_owned();
                    let succeeded = status == "succeeded";
                    return emit_ok(
                        "scan",
                        ScanOutcomeData {
                            scan_session_id,
                            status,
                            observation_count: session.observation_count,
                            retired_location_count: session.retired_location_count,
                        },
                        Some(local),
                        Vec::new(),
                    )
                    .map(|()| if succeeded { 0 } else { 2 });
                }
            }
            Err(err) => {
                let message = err.to_string();
                emit_err_with_data_and_warnings(
                    "scan",
                    serde_json::json!({ "scan_session_id": scan_session_id }),
                    err.code(),
                    message,
                    None,
                    Some(local),
                    Vec::new(),
                )?;
                return Ok(2);
            }
        }
        if std::time::Instant::now() >= deadline {
            emit_err_with_data_and_warnings(
                "scan",
                serde_json::json!({ "scan_session_id": scan_session_id }),
                ErrorCode::RequestTimeout.as_str(),
                format!("scan session {scan_session_id} did not reach a terminal state in time"),
                None,
                Some(local),
                Vec::new(),
            )?;
            return Ok(2);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Terminal scan-session statuses (mirrors the store's state machine).
fn is_terminal(status: voom_core::ScanSessionStatus) -> bool {
    matches!(
        status,
        voom_core::ScanSessionStatus::Succeeded
            | voom_core::ScanSessionStatus::Failed
            | voom_core::ScanSessionStatus::Cancelled
            | voom_core::ScanSessionStatus::Stale
    )
}

#[cfg(test)]
#[path = "scan_test.rs"]
mod tests;
