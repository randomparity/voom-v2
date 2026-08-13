use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::OperationKind;
use voom_core::{
    FileLocationId, FileVersionId, JobId, StorageRootId, TicketId, TicketOperation, VoomError,
};
use voom_events::payload::TicketCreatedPayload;
use voom_events::{Event, SubjectType};
use voom_store::repo::execution::tickets::{
    NewTicket, SqliteTicketRepo, Ticket, TicketState, WorkflowTicketIdentity,
};

use super::access_declaration::{TicketStorageSource, declaration_for};
use super::binding::{BranchContext, render_default_payload};
use super::model::{OperationNode, WorkflowPlan};
use super::ticket_payload::WorkflowTicketPayload;
use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};
use crate::workflow::execution::timing::{EffectiveTiming, branch_codec, seeded_timing};

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExpansionContext<'a> {
    pub control: &'a ControlPlane,
    pub plan: &'a WorkflowPlan,
    pub workflow_id: &'a str,
    pub plan_id: &'a str,
    pub job_id: JobId,
    pub now: OffsetDateTime,
}

impl<'a> ExpansionContext<'a> {
    #[must_use]
    pub(crate) fn new(
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

pub(crate) async fn expand_scanner_completion(
    ctx: &ExpansionContext<'_>,
    scanner_ticket: &Ticket,
) -> Result<Vec<Ticket>, VoomError> {
    let files = scanner_files(scanner_ticket)?;
    let files = files
        .into_iter()
        .take(ctx.plan.fan_out.max_files)
        .collect::<Vec<_>>();
    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let branch_ids = branch_ids_from_paths(&paths)?;
    // The scan holds a root and no location; each child's location comes from the
    // scan result entry that named it.
    let storage_root_id = parent_storage_root(&parse_workflow_payload(scanner_ticket)?)?;
    let mut specs = Vec::new();
    for (file, branch_id) in files.into_iter().zip(branch_ids) {
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
                    storage_source: Some(TicketStorageSource::Location {
                        storage_root_id,
                        file_location_id: file.file_location_id,
                    }),
                },
                scanner_ticket.id,
                scanner_ticket,
            )?);
        }
    }
    create_missing_tickets(ctx, specs).await
}

pub(crate) async fn expand_probe_completion(
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
            // The quality child scores the same file the probe read.
            storage_source: Some(parent_storage_source(&probe_payload)?),
            source_file: probe_payload.source_file,
        },
        probe_ticket.id,
        probe_ticket,
    )?;
    create_missing_tickets(ctx, vec![spec]).await
}

pub(crate) async fn expand_quality_completion(
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
            // score_quality is not byte-touching and declares nothing, but it
            // carries the source — which is what makes these children renderable.
            storage_source: Some(parent_storage_source(&quality_payload)?),
            source_file: quality_payload.source_file,
        },
        quality_ticket.id,
        quality_ticket,
    )?;
    create_missing_tickets(ctx, vec![spec]).await
}

pub(crate) async fn expand_transform_completion(
    ctx: &ExpansionContext<'_>,
    branch_id: &str,
    transform_ticket: &Ticket,
) -> Result<Vec<Ticket>, VoomError> {
    let output_path = transform_result_output_path(transform_ticket)?;
    let transform_payload = parse_workflow_payload(transform_ticket)?;
    // These children operate on the transform's output, which has no
    // file_locations row until commit creates one — so the root, not the
    // parent's location, is what they can name.
    let branch = BranchContext {
        branch_id: branch_id.to_owned(),
        path: output_path,
        probe_codec: None,
        storage_source: Some(TicketStorageSource::Root {
            storage_root_id: parent_storage_root(&transform_payload)?,
        }),
        source_file: transform_payload.source_file,
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

pub(crate) async fn expand_backup_completion(
    ctx: &ExpansionContext<'_>,
    branch_id: &str,
    backup_ticket: &Ticket,
) -> Result<Vec<Ticket>, VoomError> {
    let local_backup_id = string_result_field(backup_ticket, "local_backup_id")?;
    let backup_payload = parse_workflow_payload(backup_ticket)?;
    let spec = spec_for_branch(
        ctx,
        "verify",
        &BranchContext {
            branch_id: branch_id.to_owned(),
            path: local_backup_id,
            probe_codec: None,
            // The verify child reads the backup artifact, not the parent's source.
            storage_source: Some(TicketStorageSource::Root {
                storage_root_id: parent_storage_root(&backup_payload)?,
            }),
            source_file: backup_payload.source_file,
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
    kind: TicketOperation,
    payload: Value,
    priority: i64,
    max_attempts: u32,
    depends_on: TicketId,
    source_file_version_id: Option<FileVersionId>,
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
    let source_file_version_id = rendered_payload
        .get("source_file_version_id")
        .and_then(Value::as_u64)
        .map(FileVersionId);
    let payload = WorkflowTicketPayload {
        workflow_id: ctx.workflow_id.to_owned(),
        plan_id: ctx.plan_id.to_owned(),
        node_id: node_id.to_owned(),
        branch_id: branch.branch_id.clone(),
        operation,
        rendered_payload,
        timing,
        source_file: branch.source_file.clone(),
        declared_artifact_access: declaration_for(operation, branch.storage_source.as_ref())?,
    }
    .to_ticket_payload()
    .map_err(|e| VoomError::Config(format!("workflow ticket payload encode: {e}")))?;

    Ok(TicketSpec {
        node_id: node_id.to_owned(),
        branch_id: branch.branch_id.clone(),
        kind: ticket_kind(operation)?,
        payload,
        priority: parent_ticket.priority,
        max_attempts: parent_ticket.max_attempts,
        depends_on,
        source_file_version_id,
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
        if let Some(ticket_id) = ctx
            .control
            .tickets
            .find_workflow_ticket_id_in_tx(
                &mut tx,
                WorkflowTicketIdentity {
                    job_id: ctx.job_id,
                    workflow_id: ctx.workflow_id,
                    branch_id: &spec.branch_id,
                    node_id: &spec.node_id,
                    source_file_version_id: spec.source_file_version_id,
                },
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
                ticket_id: ticket.id,
                job_id: input.job_id,
                kind: input.kind.clone(),
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

async fn ensure_dependency_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tickets: &SqliteTicketRepo,
    ticket_id: TicketId,
    depends_on: TicketId,
) -> Result<(), VoomError> {
    if tickets
        .dependency_exists_in_tx(tx, ticket_id, depends_on)
        .await?
    {
        return Ok(());
    }

    let ticket = tickets.get_in_tx(tx, ticket_id).await?;
    match ticket {
        Some(ticket) if ticket.state == TicketState::Pending => {
            tickets
                .add_dependency_in_tx(tx, ticket_id, depends_on)
                .await
        }
        Some(ticket) => Err(VoomError::Conflict(format!(
            "workflow ticket {ticket_id} is {}; missing dependency on {depends_on} cannot be repaired",
            ticket.state.as_str()
        ))),
        None => Err(VoomError::NotFound(format!("ticket {ticket_id}"))),
    }
}

#[derive(Debug, Clone)]
struct ScannerFile {
    path: String,
    source_file: Value,
    /// A scan is the one place a child's location is discovered rather than
    /// inherited, so each entry must name one.
    file_location_id: FileLocationId,
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
            // The string form names no location, so it can no longer describe a
            // file a byte-touching child will open.
            Value::String(_) => Err(VoomError::Config(
                "scanner result file entry requires file_location_id".to_owned(),
            )),
            Value::Object(object) => {
                let path = object
                    .get("path")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        VoomError::Config("scanner result file object requires path".to_owned())
                    })?;
                let file_location_id = object
                    .get("file_location_id")
                    .and_then(Value::as_u64)
                    .filter(|id| *id != 0)
                    .map(FileLocationId)
                    .ok_or_else(|| {
                        VoomError::Config(
                            "scanner result file entry requires file_location_id".to_owned(),
                        )
                    })?;
                Ok(ScannerFile {
                    path,
                    source_file: file.clone(),
                    file_location_id,
                })
            }
            _ => Err(VoomError::Config(
                "scanner result files must be strings or objects".to_owned(),
            )),
        })
        .collect()
}

/// The source a parent ticket recorded.
///
/// Read off `rendered_payload`, never off the parent's declaration: three of the
/// five expansions build children whose bytes are not the parent's, so inheriting
/// a declaration would name the wrong bytes. The root is required because every
/// workflow ticket that has a source records one.
fn parent_storage_root(payload: &WorkflowTicketPayload) -> Result<StorageRootId, VoomError> {
    payload
        .rendered_payload
        .get("source_storage_root_id")
        .and_then(Value::as_u64)
        .filter(|id| *id != 0)
        .map(StorageRootId)
        .ok_or_else(|| {
            VoomError::Config(
                "parent ticket payload requires a non-zero source_storage_root_id".to_owned(),
            )
        })
}

/// The parent's own source, for a child that operates on the same bytes.
fn parent_storage_source(
    payload: &WorkflowTicketPayload,
) -> Result<TicketStorageSource, VoomError> {
    let storage_root_id = parent_storage_root(payload)?;
    // Absent means the parent addresses its root. Present-but-unreadable does not:
    // treating it as absent would silently widen the child from one location to
    // read+write on the whole root, and the child's own payload and declaration
    // would agree, so nothing downstream could notice.
    let Some(raw) = payload.rendered_payload.get("source_location_id") else {
        return Ok(TicketStorageSource::Root { storage_root_id });
    };
    let file_location_id = raw
        .as_u64()
        .filter(|id| *id != 0)
        .map(FileLocationId)
        .ok_or_else(|| {
            VoomError::Config(format!(
                "parent ticket payload source_location_id must be a non-zero integer, got {raw}"
            ))
        })?;
    Ok(TicketStorageSource::Location {
        storage_root_id,
        file_location_id,
    })
}

pub(crate) fn branch_id_from_path(path: &str) -> Result<String, VoomError> {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| VoomError::Config(format!("cannot derive branch id from `{path}`")))?;
    Ok(stem.to_owned())
}

pub(crate) fn branch_ids_from_paths(paths: &[String]) -> Result<Vec<String>, VoomError> {
    let mut branch_ids = Vec::with_capacity(paths.len());
    let mut indexes_by_stem: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, path) in paths.iter().enumerate() {
        let branch_id = branch_id_from_path(path)?;
        indexes_by_stem
            .entry(branch_id.clone())
            .or_default()
            .push(index);
        branch_ids.push(branch_id);
    }

    for indexes in indexes_by_stem.values() {
        if !has_distinct_paths(paths, indexes) {
            continue;
        }
        let disambiguated = branch_ids_for_colliding_paths(paths, indexes)?;
        for (index, branch_id) in indexes.iter().copied().zip(disambiguated) {
            branch_ids[index] = branch_id;
        }
    }

    ensure_unique_branch_ids_for_distinct_paths(paths, &branch_ids)?;
    Ok(branch_ids)
}

fn has_distinct_paths(paths: &[String], indexes: &[usize]) -> bool {
    let Some(first) = indexes.first().map(|index| paths[*index].as_str()) else {
        return false;
    };
    indexes.iter().any(|index| paths[*index] != first)
}

fn branch_ids_for_colliding_paths(
    paths: &[String],
    indexes: &[usize],
) -> Result<Vec<String>, VoomError> {
    let parents = indexes
        .iter()
        .map(|index| {
            Path::new(&paths[*index])
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    let common = longest_common_dir(&parents);
    indexes
        .iter()
        .map(|index| branch_id_from_relative_path(&paths[*index], &common))
        .collect()
}

fn branch_id_from_relative_path(path: &str, common_dir: &Path) -> Result<String, VoomError> {
    let path = Path::new(path);
    let relative = path.strip_prefix(common_dir).unwrap_or(path);
    let branch_id = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if branch_id.is_empty() {
        return Err(VoomError::Config(format!(
            "cannot derive disambiguated branch id from `{}`",
            path.display()
        )));
    }
    Ok(branch_id)
}

fn longest_common_dir(dirs: &[PathBuf]) -> PathBuf {
    let mut iter = dirs.iter();
    let Some(first) = iter.next() else {
        return PathBuf::new();
    };
    let mut common: Vec<Component> = first.components().collect();
    for dir in iter {
        let shared = common
            .iter()
            .zip(dir.components())
            .take_while(|(a, b)| *a == b)
            .count();
        common.truncate(shared);
    }
    common.iter().collect()
}

fn ensure_unique_branch_ids_for_distinct_paths(
    paths: &[String],
    branch_ids: &[String],
) -> Result<(), VoomError> {
    let mut paths_by_branch = HashMap::new();
    for (path, branch_id) in paths.iter().zip(branch_ids) {
        if let Some(existing_path) = paths_by_branch.insert(branch_id.as_str(), path.as_str())
            && existing_path != path
        {
            return Err(VoomError::Config(format!(
                "paths `{existing_path}` and `{path}` both derive branch id `{branch_id}`"
            )));
        }
    }
    Ok(())
}

fn operation_for_node(plan: &WorkflowPlan, node_id: &str) -> Result<OperationKind, VoomError> {
    plan.nodes
        .iter()
        .find(|node| node.id() == node_id)
        .map(OperationNode::operation)
        .ok_or_else(|| VoomError::Config(format!("workflow node `{node_id}` not found")))
}

fn parse_workflow_payload(ticket: &Ticket) -> Result<WorkflowTicketPayload, VoomError> {
    WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload.clone())
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

fn transform_result_output_path(ticket: &Ticket) -> Result<String, VoomError> {
    let result = ticket
        .result
        .as_ref()
        .ok_or_else(|| VoomError::Config(format!("ticket {} result is required", ticket.id)))?;
    if let Some(path) = result.get("output_path").and_then(Value::as_str) {
        return Ok(path.to_owned());
    }
    result
        .get("output")
        .and_then(|output| output.get("local_file_key"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            VoomError::Config(format!(
                "ticket {} result must include `output_path` or `output.local_file_key`",
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

fn ticket_kind(operation: OperationKind) -> Result<TicketOperation, VoomError> {
    TicketOperation::new(format!(
        "synthetic.workflow.operation.{}",
        operation.as_str()
    ))
}

#[cfg(test)]
#[path = "expansion_test.rs"]
mod tests;
