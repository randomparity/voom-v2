use std::path::Path;

use serde_json::Value;
use time::OffsetDateTime;
use voom_core::{JobId, TicketId, VoomError};
use voom_events::payload::TicketCreatedPayload;
use voom_events::{Event, SubjectType};
use voom_store::repo::tickets::{NewTicket, Ticket, TicketRepo};
use voom_worker_protocol::OperationKind;

use super::binding::{BranchContext, render_default_payload};
use super::model::{WorkflowNode, WorkflowPlan};
use super::ticket_payload::{WorkflowTicketPayload, operation_name};
use super::timing::EffectiveTiming;
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
    for path in files {
        let branch_id = branch_id_from_path(&path)?;
        for node_id in ["probe", "hash", "identity"] {
            specs.push(spec_for_branch(
                ctx,
                node_id,
                &BranchContext {
                    branch_id: branch_id.clone(),
                    path: path.clone(),
                    probe_codec: None,
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
    let spec = spec_for_branch(
        ctx,
        "backup",
        &BranchContext {
            branch_id: branch_id.to_owned(),
            path: output_path,
            probe_codec: None,
        },
        transform_ticket.id,
        transform_ticket,
    )?;
    create_missing_tickets(ctx, vec![spec]).await
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
    let rendered_payload = render_default_payload(operation, branch, timing(ctx))
        .map_err(|e| VoomError::Config(format!("workflow payload binding: {e}")))?;
    let payload = WorkflowTicketPayload {
        workflow_id: ctx.workflow_id.to_owned(),
        plan_id: ctx.plan_id.to_owned(),
        node_id: node_id.to_owned(),
        branch_id: branch.branch_id.clone(),
        operation,
        rendered_payload,
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
    let mut missing = Vec::new();
    for spec in specs {
        if find_existing_ticket(ctx, &spec.kind, &spec.branch_id, &spec.node_id)
            .await?
            .is_none()
        {
            missing.push(spec);
        }
    }

    if missing.is_empty() {
        return Ok(Vec::new());
    }

    let mut tx = begin_tx(&ctx.control.pool).await?;
    let mut created = Vec::new();
    for spec in missing {
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
        created.push(ticket);
    }
    commit_tx(tx).await?;

    let mut refreshed = Vec::new();
    for ticket in created {
        ctx.control
            .mark_ready_if_unblocked(ticket.id, ctx.now)
            .await?;
        let ticket =
            ctx.control.tickets.get(ticket.id).await?.ok_or_else(|| {
                VoomError::Internal(format!("created ticket {} vanished", ticket.id))
            })?;
        refreshed.push(ticket);
    }
    Ok(refreshed)
}

async fn find_existing_ticket(
    ctx: &ExpansionContext<'_>,
    kind: &str,
    branch_id: &str,
    node_id: &str,
) -> Result<Option<Ticket>, VoomError> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM tickets \
         WHERE job_id = ? \
           AND kind = ? \
           AND json_extract(payload, '$.branch_id') = ? \
           AND json_extract(payload, '$.node_id') = ? \
         ORDER BY id ASC \
         LIMIT 1",
    )
    .bind(sqlite_i64(ctx.job_id.0, "job id")?)
    .bind(kind)
    .bind(branch_id)
    .bind(node_id)
    .fetch_optional(&ctx.control.pool)
    .await
    .map_err(|e| VoomError::Database(format!("workflow ticket lookup: {e}")))?;

    let Some((id,)) = row else {
        return Ok(None);
    };
    ctx.control
        .tickets
        .get(TicketId(sqlite_u64(id, "ticket id")?))
        .await
}

fn scanner_files(scanner_ticket: &Ticket) -> Result<Vec<String>, VoomError> {
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
            Value::String(path) => Ok(path.clone()),
            Value::Object(object) => object
                .get("path")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    VoomError::Config("scanner result file object requires path".to_owned())
                }),
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

fn timing(ctx: &ExpansionContext<'_>) -> EffectiveTiming {
    EffectiveTiming {
        duration_ms: ctx.plan.timing.base_duration_ms,
        progress_interval_ms: ctx.plan.timing.jitter_ms,
    }
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
