use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_core::UseLeaseId;
use voom_store::repo::{
    BlockingMode, IssuerKind, NewUseLease, UseLease, UseLeaseKind, UseLeaseReleaseReason,
};

use crate::cli::LeaseCommand;
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_ok};

const COMMAND: &str = "lease";

/// Wire projection of one `asset_use_leases` row. Timestamps are ISO-8601;
/// the four exclusive `scope_*_id` columns collapse into `scope_type` +
/// `scope_id`.
#[derive(Debug, Serialize)]
pub struct LeaseWire {
    pub id: u64,
    pub kind: String,
    pub scope_type: String,
    pub scope_id: u64,
    pub issuer_kind: String,
    pub issuer_ref: String,
    pub blocking_mode: String,
    pub ttl_bound: bool,
    pub acquired_at: String,
    pub expires_at: Option<String>,
    pub released_at: Option<String>,
    pub release_reason: Option<String>,
    pub epoch: u64,
}

impl From<UseLease> for LeaseWire {
    fn from(lease: UseLease) -> Self {
        Self {
            id: lease.id.0,
            kind: lease.kind.as_str().to_owned(),
            scope_type: lease.scope.type_str().to_owned(),
            scope_id: lease.scope.id_u64(),
            issuer_kind: lease.issuer_kind.as_str().to_owned(),
            issuer_ref: lease.issuer_ref,
            blocking_mode: lease.blocking_mode.as_str().to_owned(),
            ttl_bound: lease.ttl_bound,
            acquired_at: voom_core::format_iso8601(lease.acquired_at),
            expires_at: lease.expires_at.map(voom_core::format_iso8601),
            released_at: lease.released_at.map(voom_core::format_iso8601),
            release_reason: lease.release_reason.map(|r| r.as_str().to_owned()),
            epoch: lease.epoch,
        }
    }
}

/// A live manual lock plus its age in whole seconds against the control-plane
/// clock — surfaces forgotten holds in `voom lease list`.
#[derive(Debug, Serialize)]
pub struct LeaseListEntry {
    #[serde(flatten)]
    pub lease: LeaseWire,
    pub age_seconds: i64,
}

#[derive(Debug, Serialize)]
pub struct LeaseListData {
    pub locks: Vec<LeaseListEntry>,
}

pub async fn run(database_url: &str, local: Local, command: LeaseCommand) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        LeaseCommand::Acquire {
            scope_type,
            scope_id,
            issuer_ref,
        } => {
            if issuer_ref.trim().is_empty() {
                // A blank holder defeats the forgotten-hold spotting `lease
                // list` exists for, so reject it at the operator boundary.
                return emit_voom_error(
                    COMMAND,
                    &voom_core::VoomError::Config(
                        "issuer_ref must not be empty or whitespace".to_owned(),
                    ),
                    local,
                );
            }
            let now = cp.clock().now();
            let input = NewUseLease {
                kind: UseLeaseKind::ManualLock,
                scope: scope_type.to_scope(scope_id),
                issuer_kind: IssuerKind::User,
                issuer_ref,
                blocking_mode: BlockingMode::Blocking,
                ttl: None,
                acquired_at: now,
            };
            emit_one(cp.acquire_use_lease(input).await, local)
        }
        LeaseCommand::Release { lease_id } => {
            let now = cp.clock().now();
            emit_one(
                cp.release_use_lease(UseLeaseId(lease_id), UseLeaseReleaseReason::Released, now)
                    .await,
                local,
            )
        }
        LeaseCommand::ForceRelease {
            lease_id,
            actor,
            reason,
        } => {
            let now = cp.clock().now();
            emit_one(
                cp.force_release_use_lease(UseLeaseId(lease_id), actor, reason, now)
                    .await,
                local,
            )
        }
        LeaseCommand::List => list(&cp, local).await,
    }
}

async fn list(cp: &ControlPlane, local: Local) -> io::Result<i32> {
    let now = cp.clock().now();
    match cp.list_manual_locks().await {
        Ok(leases) => {
            let locks = leases
                .into_iter()
                .map(|lease| LeaseListEntry {
                    age_seconds: (now - lease.acquired_at).whole_seconds(),
                    lease: LeaseWire::from(lease),
                })
                .collect();
            emit_ok(COMMAND, LeaseListData { locks }, Some(local), Vec::new()).map(|()| 0)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_one(result: Result<UseLease, voom_core::VoomError>, local: Local) -> io::Result<i32> {
    match result {
        Ok(lease) => emit_ok(COMMAND, LeaseWire::from(lease), Some(local), Vec::new()).map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}
