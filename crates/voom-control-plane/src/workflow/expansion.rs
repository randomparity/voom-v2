use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;
use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{JobId, TicketId, VoomError};
use voom_events::payload::TicketCreatedPayload;
use voom_events::{Event, SubjectType};
use voom_store::repo::tickets::{NewTicket, Ticket, TicketRepo, TicketState};
use voom_worker_protocol::OperationKind;

use super::binding::{BranchContext, render_default_payload};
use super::model::{WorkflowNode, WorkflowPlan};
use super::ticket_payload::{WorkflowTicketPayload, operation_name};
use super::timing::{EffectiveTiming, branch_codec, seeded_timing};
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};

#[derive(Debug, Clone, Copy)]
pub struct ExpansionContext<'a> {
    pub control: &'a ControlPlane,
    pub plan: &'a WorkflowPlan,
    pub workflow_id: &'a str,
    pub plan_id: &'a str,
    pub job_id: JobId,
    pub now: OffsetDateTime,
}

impl<'a> ExpansionContext<'a> {
    #[must_use]
    pub fn new(
        control: &'a ControlPlane,
        plan: &'a WorkflowPlan,
        workflow_id: &'a str,
        plan_id: &'a str,
        job_id: JobId,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            control,
            plan,
            workflow_id,
            plan_id,
            job_id,
            now,
        }
    }
}

pub async fn expand_scanner_completion(
    ctx: &ExpansionContext<'_>,
    scanner_ticket: &Ticket,
) -> Result<Vec<Ticket>, VoomError> {
    let files = scanner_files(scanner_ticket)?;
    let files = files.into_iter().take(ctx.plan.fan_out.max_files);
    let mut specs = Vec::new();
    let mut paths_by_branch = HashMap::new();
    for file in files {
        let branch_id = branch_id_from_path(&file.path)?;
        if let Some(existing_path) = paths_by_branch.insert(branch_id.clone(), file.path.clone())
            && existing_path != file.path
        {
            return Err(VoomError::Config(format!(
                "scanner paths `{existing_path}` and `{}` both derive branch id `{branch_id}`",
                file.path
            )));
        }
        for node_id in ["probe", "hash", "identity"] {
            specs.push(spec_for_branch(
                ctx,
                node_id,
                &BranchContext {
                    branch_id: branch_id.clone(),
                    path: file.path.clone(),
                    probe_codec: (node_id == "probe")
                        .then(|| branch_codec(ctx.plan.seed, &branch_id).to_owned()),
                    source_file: Some(file.source_file.clone()),
                },
                scanner_ticket.id,
                scanner_ticket,
            )?);
        }
    }
    create_missing_tickets(ctx, specs).await
}

pub async fn expand_probe_completion(
    ctx: &ExpansionContext<'_>,
    branch_id: &str,
    probe_ticket: &Ticket,
) -> Result<Vec<Ticket>, VoomError> {
    let probe_payload = parse_workflow_payload(probe_ticket)?;
    let path = rendered_path(&probe_payload)?;
    let codec = string_result_field(probe_ticket, "codec")?;
    let spec = spec_for_branch(
        ctx,
        "quality",
        &BranchContext {
            branch_id: branch_id.to_owned(),
            path,
            probe_codec: Some(codec),
            source_file: probe_payload.source_file,
        },
        probe_ticket.id,
        probe_ticket,
    )?;
    create_missing_tickets(ctx, vec![spec]).await
}

pub async fn expand_quality_completion(
    ctx: &ExpansionContext<'_>,
    branch_id: &str,
    quality_ticket: &Ticket,
) -> Result<Vec<Ticket>, VoomError> {
    let quality_payload = parse_workflow_payload(quality_ticket)?;
    let needs_transcode = bool_result_field(quality_ticket, "needs_transcode")?;
    let node_id = if needs_transcode {
        "transcode"
    } else {
        "remux"
    };
    let spec = spec_for_branch(
        ctx,
        node_id,
        &BranchContext {
            branch_id: branch_id.to_owned(),
            path: rendered_path(&quality_payload)?,
            probe_codec: None,
            source_file: quality_payload.source_file,
        },
        quality_ticket.id,
        quality_ticket,
    )?;
    create_missing_tickets(ctx, vec![spec]).await
}

pub async fn expand_transform_completion(
    ctx: &ExpansionContext<'_>,
    branch_id: &str,
    transform_ticket: &Ticket,
) -> Result<Vec<Ticket>, VoomError> {
    let output_path = string_result_field(transform_ticket, "output_path")?;
    let branch = BranchContext {
        branch_id: branch_id.to_owned(),
        path: output_path,
        probe_codec: None,
        source_file: parse_workflow_payload(transform_ticket)?.source_file,
    };
    let mut specs = Vec::new();
    for node_id in ["backup", "external-sync", "issue", "use-lease"] {
        specs.push(spec_for_branch(
            ctx,
            node_id,
            &branch,
            transform_ticket.id,
            transform_ticket,
        )?);
    }
    create_missing_tickets(ctx, specs).await
}

pub async fn expand_backup_completion(
    ctx: &ExpansionContext<'_>,
    branch_id: &str,
    backup_ticket: &Ticket,
) -> Result<Vec<Ticket>, VoomError> {
    let local_backup_id = string_result_field(backup_ticket, "local_backup_id")?;
    let spec = spec_for_branch(
        ctx,
        "verify",
        &BranchContext {
            branch_id: branch_id.to_owned(),
            path: local_backup_id,
            probe_codec: None,
            source_file: parse_workflow_payload(backup_ticket)?.source_file,
        },
        backup_ticket.id,
        backup_ticket,
    )?;
    create_missing_tickets(ctx, vec![spec]).await
}

#[derive(Debug)]
struct TicketSpec {
    node_id: String,
    branch_id: String,
    kind: String,
    payload: Value,
    priority: i64,
    max_attempts: u32,
    depends_on: TicketId,
}

fn spec_for_branch(
    ctx: &ExpansionContext<'_>,
    node_id: &str,
    branch: &BranchContext,
    depends_on: TicketId,
    parent_ticket: &Ticket,
) -> Result<TicketSpec, VoomError> {
    let operation = operation_for_node(ctx.plan, node_id)?;
    let timing = timing(ctx, node_id, &branch.branch_id);
    let rendered_payload = render_default_payload(operation, branch, timing)
        .map_err(|e| VoomError::Config(format!("workflow payload binding: {e}")))?;
    let payload = WorkflowTicketPayload {
        workflow_id: ctx.workflow_id.to_owned(),
        plan_id: ctx.plan_id.to_owned(),
        node_id: node_id.to_owned(),
        branch_id: branch.branch_id.clone(),
        operation,
        rendered_payload,
        timing,
        source_file: branch.source_file.clone(),
    }
    .to_ticket_payload()
    .map_err(|e| VoomError::Config(format!("workflow ticket payload encode: {e}")))?;

    Ok(TicketSpec {
        node_id: node_id.to_owned(),
        branch_id: branch.branch_id.clone(),
        kind: ticket_kind(operation),
        payload,
        priority: parent_ticket.priority,
        max_attempts: parent_ticket.max_attempts,
        depends_on,
    })
}

async fn create_missing_tickets(
    ctx: &ExpansionContext<'_>,
    specs: Vec<TicketSpec>,
) -> Result<Vec<Ticket>, VoomError> {
    let specs = dedupe_specs(specs);

    let mut tx = begin_tx(&ctx.control.pool).await?;
    let mut expected_ids = Vec::new();
    let mut created_ids = Vec::new();
    for spec in specs {
        if let Some(ticket_id) = find_existing_ticket_id_in_tx(
            &mut tx,
            ctx.job_id,
            &spec.kind,
            &spec.branch_id,
            &spec.node_id,
        )
        .await?
        {
            ensure_dependency_in_tx(&mut tx, &ctx.control.tickets, ticket_id, spec.depends_on)
                .await?;
            expected_ids.push(ticket_id);
            continue;
        }
        let input = NewTicket {
            job_id: Some(ctx.job_id),
            kind: spec.kind,
            priority: spec.priority,
            payload: spec.payload,
            max_attempts: spec.max_attempts,
            created_at: ctx.now,
        };
        let ticket = ctx
            .control
            .tickets
            .create_in_tx(&mut tx, input.clone())
            .await?;
        append_event(
            &ctx.control.events,
            &mut tx,
            SubjectType::Ticket,
            Some(ticket.id.0),
            input.created_at,
            Event::TicketCreated(TicketCreatedPayload {
                ticket_id: ticket.id.0,
                job_id: input.job_id.map(|job_id| job_id.0),
                kind: input.kind,
                priority: input.priority,
                max_attempts: input.max_attempts,
            }),
        )
        .await?;
        ctx.control
            .tickets
            .add_dependency_in_tx(&mut tx, ticket.id, spec.depends_on)
            .await?;
        expected_ids.push(ticket.id);
        created_ids.push(ticket.id);
    }
    commit_tx(tx).await?;

    for ticket_id in expected_ids {
        ctx.control
            .mark_ready_if_unblocked(ticket_id, ctx.now)
            .await?;
    }

    let mut refreshed = Vec::new();
    for ticket_id in created_ids {
        let ticket =
            ctx.control.tickets.get(ticket_id).await?.ok_or_else(|| {
                VoomError::Internal(format!("created ticket {ticket_id} vanished"))
            })?;
        refreshed.push(ticket);
    }
    Ok(refreshed)
}

fn dedupe_specs(specs: Vec<TicketSpec>) -> Vec<TicketSpec> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for spec in specs {
        let key = (
            spec.kind.clone(),
            spec.branch_id.clone(),
            spec.node_id.clone(),
        );
        if seen.insert(key) {
            out.push(spec);
        }
    }
    out
}

async fn find_existing_ticket_id_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    job_id: JobId,
    kind: &str,
    branch_id: &str,
    node_id: &str,
) -> Result<Option<TicketId>, VoomError> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT id FROM tickets \
         WHERE job_id = ? \
           AND kind = ? \
           AND json_extract(payload, '$.branch_id') = ? \
           AND json_extract(payload, '$.node_id') = ? \
         ORDER BY id ASC \
         LIMIT 2",
    )
    .bind(sqlite_i64(job_id.0, "job id")?)
    .bind(kind)
    .bind(branch_id)
    .bind(node_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("workflow ticket lookup: {e}")))?;

    match rows.as_slice() {
        [] => Ok(None),
        [(id,)] => Ok(Some(TicketId(sqlite_u64(*id, "ticket id")?))),
        [(first,), (second,)] => Err(VoomError::Conflict(format!(
            "duplicate workflow tickets for job {job_id} kind `{kind}` branch `{branch_id}` node `{node_id}`: ids {first}, {second}"
        ))),
        _ => unreachable!("lookup query is limited to two rows"),
    }
}

async fn ensure_dependency_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tickets: &impl TicketRepo,
    ticket_id: TicketId,
    depends_on: TicketId,
) -> Result<(), VoomError> {
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM ticket_dependencies \
         WHERE ticket_id = ? AND depends_on_ticket_id = ? \
         LIMIT 1",
    )
    .bind(sqlite_i64(ticket_id.0, "ticket id")?)
    .bind(sqlite_i64(depends_on.0, "dependency ticket id")?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("workflow dependency lookup: {e}")))?;
    if exists.is_some() {
        return Ok(());
    }

    let state: Option<String> = sqlx::query_scalar("SELECT state FROM tickets WHERE id = ?")
        .bind(sqlite_i64(ticket_id.0, "ticket id")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("workflow ticket state lookup: {e}")))?;
    match state.as_deref() {
        Some(state) if state == TicketState::Pending.as_str() => {
            tickets
                .add_dependency_in_tx(tx, ticket_id, depends_on)
                .await
        }
        Some(state) => Err(VoomError::Conflict(format!(
            "workflow ticket {ticket_id} is {state}; missing dependency on {depends_on} cannot be repaired"
        ))),
        None => Err(VoomError::NotFound(format!("ticket {ticket_id}"))),
    }
}

#[derive(Debug, Clone)]
struct ScannerFile {
    path: String,
    source_file: Value,
}

fn scanner_files(scanner_ticket: &Ticket) -> Result<Vec<ScannerFile>, VoomError> {
    let result = scanner_ticket
        .result
        .as_ref()
        .ok_or_else(|| VoomError::Config("scanner ticket result is required".to_owned()))?;
    let files = result
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| VoomError::Config("scanner result.files must be an array".to_owned()))?;
    files
        .iter()
        .map(|file| match file {
            Value::String(path) => Ok(ScannerFile {
                path: path.clone(),
                source_file: serde_json::json!({ "path": path }),
            }),
            Value::Object(object) => {
                let path = object
                    .get("path")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        VoomError::Config("scanner result file object requires path".to_owned())
                    })?;
                Ok(ScannerFile {
                    path,
                    source_file: file.clone(),
                })
            }
            _ => Err(VoomError::Config(
                "scanner result files must be strings or objects".to_owned(),
            )),
        })
        .collect()
}

fn branch_id_from_path(path: &str) -> Result<String, VoomError> {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| VoomError::Config(format!("cannot derive branch id from `{path}`")))?;
    Ok(stem.to_owned())
}

fn operation_for_node(plan: &WorkflowPlan, node_id: &str) -> Result<OperationKind, VoomError> {
    plan.nodes
        .iter()
        .find(|node| node.id() == node_id)
        .map(WorkflowNode::operation)
        .ok_or_else(|| VoomError::Config(format!("workflow node `{node_id}` not found")))
}

fn parse_workflow_payload(ticket: &Ticket) -> Result<WorkflowTicketPayload, VoomError> {
    WorkflowTicketPayload::parse_ticket(&ticket.kind, ticket.payload.clone())
        .map_err(|e| VoomError::Config(format!("workflow ticket payload decode: {e}")))
}

fn rendered_path(payload: &WorkflowTicketPayload) -> Result<String, VoomError> {
    payload
        .rendered_payload
        .get("path")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            VoomError::Config(format!(
                "rendered payload path missing for node `{}` branch `{}`",
                payload.node_id, payload.branch_id
            ))
        })
}

fn string_result_field(ticket: &Ticket, field: &str) -> Result<String, VoomError> {
    ticket
        .result
        .as_ref()
        .and_then(|result| result.get(field))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            VoomError::Config(format!(
                "ticket {} result field `{field}` must be a string",
                ticket.id
            ))
        })
}

fn bool_result_field(ticket: &Ticket, field: &str) -> Result<bool, VoomError> {
    ticket
        .result
        .as_ref()
        .and_then(|result| result.get(field))
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            VoomError::Config(format!(
                "ticket {} result field `{field}` must be a bool",
                ticket.id
            ))
        })
}

fn timing(ctx: &ExpansionContext<'_>, node_id: &str, branch_id: &str) -> EffectiveTiming {
    seeded_timing(
        ctx.plan.seed,
        node_id,
        branch_id,
        ctx.plan.timing.base_duration_ms,
        ctx.plan.timing.jitter_ms,
    )
}

fn ticket_kind(operation: OperationKind) -> String {
    format!("synthetic.workflow.operation.{}", operation_name(operation))
}

fn sqlite_i64(value: u64, field: &str) -> Result<i64, VoomError> {
    i64::try_from(value)
        .map_err(|e| VoomError::Database(format!("{field} {value} does not fit SQLite i64: {e}")))
}

fn sqlite_u64(value: i64, field: &str) -> Result<u64, VoomError> {
    u64::try_from(value)
        .map_err(|e| VoomError::Database(format!("{field} {value} does not fit u64: {e}")))
}
