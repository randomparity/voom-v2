//! Worker-lifecycle use cases. Each method opens a transaction, calls the
//! `SqliteWorkerRepo` `_in_tx` form, emits the matching event via
//! `EventRepo::append_in_tx`, then commits.

use secrecy::{ExposeSecret, SecretString};
use serde_json::Value as JsonValue;
use sqlx::{Sqlite, Transaction};
use time::Duration;
use time::OffsetDateTime;
use voom_core::{NodeId, TicketOperation, VoomError, WorkerId, WorkerKind};
use voom_events::payload::{
    WorkerCapabilityRecordedPayload, WorkerGrantRecordedPayload, WorkerLinkedToNodePayload,
    WorkerRegisteredPayload, WorkerRetiredPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::execution::nodes::NodeStatus;
use voom_store::repo::execution::workers::{
    Capability, Grant, NewCapability, NewGrant, NewWorker, Worker, WorkerInspection, WorkerStatus,
};

use crate::ControlPlane;
use crate::node_auth::verify_node_token;

use super::{append_event, begin_immediate_tx, begin_tx, commit_tx};

const BUILTIN_FFPROBE_NAME: &str = "builtin.ffprobe";
const LOCAL_FFMPEG_NAME: &str = "local-ffmpeg";
const LOCAL_MKVTOOLNIX_NAME: &str = "local-mkvtoolnix";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReservedProvider {
    Ffmpeg,
    Mkvtoolnix,
    Ffprobe,
}

pub(crate) fn reserved_provider(name: &str) -> Option<ReservedProvider> {
    if is_name_or_incarnation(name, LOCAL_FFMPEG_NAME) {
        return Some(ReservedProvider::Ffmpeg);
    }
    if is_name_or_incarnation(name, LOCAL_MKVTOOLNIX_NAME) {
        return Some(ReservedProvider::Mkvtoolnix);
    }
    if is_name_or_incarnation(name, BUILTIN_FFPROBE_NAME) {
        return Some(ReservedProvider::Ffprobe);
    }
    None
}

fn is_name_or_incarnation(name: &str, base: &str) -> bool {
    name == base
        || name
            .strip_prefix(base)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

/// Caller-supplied fields for generic, node-less worker registration.
#[derive(Debug)]
pub struct RegisterWorkerInput {
    pub name: String,
    pub kind: WorkerKind,
}

#[derive(Debug)]
pub struct RegisterWorkerForNodeInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub name: String,
    pub kind: WorkerKind,
    pub capabilities: Vec<NewWorkerCapabilityDraft>,
    pub grants: Vec<NewWorkerGrantDraft>,
}

#[derive(Debug, Clone)]
pub struct NewWorkerCapabilityDraft {
    pub operation: TicketOperation,
    pub codecs: Vec<String>,
    pub hardware: Vec<String>,
    pub artifact_access: Vec<String>,
    pub extra: JsonValue,
}

#[derive(Debug, Clone)]
pub struct NewWorkerGrantDraft {
    pub can_execute: Vec<TicketOperation>,
    pub can_access_read: Vec<String>,
    pub can_access_write: Vec<String>,
    pub denies: Vec<TicketOperation>,
    pub max_parallel: JsonValue,
}

impl ControlPlane {
    /// Register a worker and emit `worker.registered`.
    ///
    /// # Errors
    /// Propagates `SqliteWorkerRepo::register_in_tx` and event-append errors.
    pub async fn register_worker(&self, input: RegisterWorkerInput) -> Result<Worker, VoomError> {
        reject_reserved_provider(&input.name)?;
        let registered_at = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let worker = self
            .register_worker_with_event_in_tx(
                &mut tx,
                NewWorker {
                    name: input.name,
                    kind: input.kind,
                    registered_at,
                    node_id: None,
                },
            )
            .await?;
        commit_tx(tx).await?;
        Ok(worker)
    }

    #[cfg(test)]
    pub(crate) async fn register_supervisor_worker(
        &self,
        input: NewWorker,
    ) -> Result<Worker, VoomError> {
        validate_supervisor_worker(&input)?;
        let mut tx = begin_tx(&self.pool).await?;
        let worker = self
            .register_worker_with_event_in_tx(&mut tx, input)
            .await?;
        commit_tx(tx).await?;
        Ok(worker)
    }

    pub(crate) async fn register_supervisor_worker_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewWorker,
    ) -> Result<Worker, VoomError> {
        validate_supervisor_worker(&input)?;
        self.register_worker_with_event_in_tx(tx, input).await
    }

    async fn register_worker_with_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: NewWorker,
    ) -> Result<Worker, VoomError> {
        let registered_at = input.registered_at;
        let worker = self.workers.register_in_tx(tx, input).await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Worker,
            Some(worker.id.0),
            registered_at,
            Event::WorkerRegistered(WorkerRegisteredPayload {
                worker_id: worker.id,
                name: worker.name.clone(),
                kind: worker.kind,
            }),
        )
        .await?;
        Ok(worker)
    }

    /// Register a worker for a live node after verifying the node token.
    ///
    /// # Errors
    /// Returns `NOT_FOUND` for missing nodes, `CONFLICT` for invalid tokens
    /// or non-live nodes, and otherwise propagates repository/event errors.
    pub async fn register_worker_for_node(
        &self,
        input: RegisterWorkerForNodeInput,
    ) -> Result<Worker, VoomError> {
        reject_reserved_provider(&input.name)?;
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let auth = self
            .nodes
            .auth_record_in_tx(&mut tx, input.node_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!(
                    "workers register for node: id={} not found",
                    input.node_id
                ))
            })?;
        if !verify_node_token(input.token.expose_secret(), &auth.auth_token_hash) {
            return Err(VoomError::Conflict(
                "node token verification failed".to_owned(),
            ));
        }
        if matches!(auth.status, NodeStatus::Stale | NodeStatus::Retired) {
            return Err(VoomError::Conflict(format!(
                "workers register for node rejected: id={} status={}",
                input.node_id,
                auth.status.as_str()
            )));
        }
        let expires_at =
            auth.last_seen_at + Duration::seconds(i64::from(auth.heartbeat_ttl_seconds));
        if expires_at <= now {
            return Err(VoomError::Conflict(format!(
                "workers register for node rejected: id={} heartbeat expired",
                input.node_id
            )));
        }

        let worker = self
            .workers
            .register_in_tx(
                &mut tx,
                NewWorker {
                    name: input.name,
                    kind: input.kind,
                    registered_at: now,
                    node_id: Some(input.node_id),
                },
            )
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Worker,
            Some(worker.id.0),
            now,
            Event::WorkerRegistered(WorkerRegisteredPayload {
                worker_id: worker.id,
                name: worker.name.clone(),
                kind: worker.kind,
            }),
        )
        .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Worker,
            Some(worker.id.0),
            now,
            Event::WorkerLinkedToNode(WorkerLinkedToNodePayload {
                worker_id: worker.id,
                node_id: input.node_id,
            }),
        )
        .await?;
        self.record_capability_drafts(&mut tx, worker.id, input.capabilities, now)
            .await?;
        self.record_grant_drafts(&mut tx, worker.id, input.grants, now)
            .await?;
        commit_tx(tx).await?;
        Ok(worker)
    }

    /// Fetch a worker with nullable node context for inspection surfaces.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_worker_inspection(
        &self,
        worker_id: WorkerId,
    ) -> Result<Option<WorkerInspection>, VoomError> {
        self.workers.get_inspection(worker_id).await
    }

    /// List workers with nullable node context for inspection surfaces.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_worker_inspections(
        &self,
        status: Option<WorkerStatus>,
        limit: u32,
    ) -> Result<Vec<WorkerInspection>, VoomError> {
        self.workers.list_inspections(status, limit).await
    }

    async fn record_capability_drafts(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        worker_id: WorkerId,
        drafts: Vec<NewWorkerCapabilityDraft>,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        for draft in drafts {
            let operation = draft.operation.clone();
            let cap = self
                .workers
                .record_capability_in_tx(
                    tx,
                    NewCapability {
                        worker_id,
                        operation,
                        codecs: draft.codecs,
                        hardware: draft.hardware,
                        artifact_access: draft.artifact_access,
                        extra: draft.extra,
                    },
                )
                .await?;
            append_event(
                &self.events,
                tx,
                SubjectType::Worker,
                Some(worker_id.0),
                now,
                Event::WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload {
                    worker_id,
                    capability_id: cap.id,
                    operation: cap.operation,
                }),
            )
            .await?;
        }
        Ok(())
    }

    async fn record_grant_drafts(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        worker_id: WorkerId,
        drafts: Vec<NewWorkerGrantDraft>,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        for draft in drafts {
            let grant = self
                .workers
                .record_grant_in_tx(
                    tx,
                    NewGrant {
                        worker_id,
                        can_execute: draft.can_execute,
                        can_access_read: draft.can_access_read,
                        can_access_write: draft.can_access_write,
                        denies: draft.denies,
                        max_parallel: draft.max_parallel,
                    },
                )
                .await?;
            append_event(
                &self.events,
                tx,
                SubjectType::Worker,
                Some(worker_id.0),
                now,
                Event::WorkerGrantRecorded(WorkerGrantRecordedPayload {
                    worker_id,
                    grant_id: grant.id,
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// Record a worker capability and emit `worker.capability_recorded`.
    ///
    /// # Errors
    /// Propagates `SqliteWorkerRepo::record_capability_in_tx` and event-append errors.
    pub async fn record_capability(&self, input: NewCapability) -> Result<Capability, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let worker_id = input.worker_id;
        let operation = input.operation.clone();
        let cap = self.workers.record_capability_in_tx(&mut tx, input).await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Worker,
            Some(worker_id.0),
            self.clock().now(),
            Event::WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload {
                worker_id,
                capability_id: cap.id,
                operation,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(cap)
    }

    /// Record a worker grant and emit `worker.grant_recorded`.
    ///
    /// # Errors
    /// Propagates `SqliteWorkerRepo::record_grant_in_tx` and event-append errors.
    pub async fn record_grant(&self, input: NewGrant) -> Result<Grant, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let worker_id = input.worker_id;
        let grant = self.workers.record_grant_in_tx(&mut tx, input).await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Worker,
            Some(worker_id.0),
            self.clock().now(),
            Event::WorkerGrantRecorded(WorkerGrantRecordedPayload {
                worker_id,
                grant_id: grant.id,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(grant)
    }

    /// Retire a worker and emit `worker.retired`.
    ///
    /// # Errors
    /// Propagates `SqliteWorkerRepo::retire_in_tx` and event-append errors.
    pub async fn retire_worker(
        &self,
        id: WorkerId,
        expected_epoch: u64,
        now: OffsetDateTime,
    ) -> Result<Worker, VoomError> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let worker = self
            .workers
            .retire_in_tx(&mut tx, id, expected_epoch, now)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Worker,
            Some(id.0),
            now,
            Event::WorkerRetired(WorkerRetiredPayload { worker_id: id }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(worker)
    }
}

fn reject_reserved_provider(name: &str) -> Result<(), VoomError> {
    let Some(provider) = reserved_provider(name) else {
        return Ok(());
    };
    Err(VoomError::Conflict(format!(
        "worker name {name:?} is reserved for the {provider:?} supervisor"
    )))
}

fn validate_supervisor_worker(input: &NewWorker) -> Result<(), VoomError> {
    if reserved_provider(&input.name).is_none() {
        return Err(VoomError::Conflict(format!(
            "supervisor worker name {:?} is not reserved",
            input.name
        )));
    }
    if input.kind != WorkerKind::Local || input.node_id.is_some() {
        return Err(VoomError::Conflict(format!(
            "supervisor worker {:?} must be node-less and local",
            input.name
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
