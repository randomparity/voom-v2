#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]
//! Shared helpers for Sprint 2 fake-provider binaries.
//!
//! Consumed only by the eleven `fake-*` binaries in `voom-fakes`.
//! `chaos-worker`, `benchmark-worker`, and `voom-conformance` do
//! NOT depend on this crate -- keeping their behavior independent
//! of any shared encoder/decoder bug.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use voom_worker_protocol::{
    HttpServer, OperationDispatch, OperationFuture, OperationKind, OperationRequest,
    OperationResponse, PercentBps, ProgressFrame, ProtocolError, ServerHandle, WorkerCredentials,
};

const MAX_FAKE_DURATION_MS: u64 = 30_000;

#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("read: {0}")]
    Read(String),
    #[error("decode: {0}")]
    Decode(String),
}

/// One scripted event a fake's operation handler consumes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioEvent {
    DiscoverFile {
        path: String,
        size: u64,
    },
    ScanComplete {
        duration_ms: u32,
    },
    Custom {
        name: String,
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub scenario: String,
    pub events: Vec<ScenarioEvent>,
}

pub fn load_scenario(path: impl AsRef<Path>) -> Result<Scenario, ScenarioError> {
    let bytes = std::fs::read(path.as_ref()).map_err(|e| ScenarioError::Read(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| ScenarioError::Decode(e.to_string()))
}

#[derive(Debug, Clone)]
pub struct ScenarioPlayer {
    events: std::vec::IntoIter<ScenarioEvent>,
}

impl ScenarioPlayer {
    #[must_use]
    pub fn new(scenario: Scenario) -> Self {
        Self {
            events: scenario.events.into_iter(),
        }
    }

    pub fn next_event(&mut self) -> Option<ScenarioEvent> {
        self.events.next()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderDefinition {
    pub binary_name: &'static str,
    pub provider: &'static str,
    pub primary: OperationKind,
    pub secondary: &'static [OperationKind],
}

#[derive(Debug, Clone, Copy)]
enum ProviderKind {
    Scanner,
    Prober,
    Transcoder,
    Remuxer,
    BackupStore,
    HealthChecker,
    IdentityProvider,
    ExternalSystem,
    QualityScorer,
    IssueProvider,
    UseLeaseProvider,
}

#[derive(Debug, Clone, Copy)]
struct ProviderCatalogEntry {
    definition: ProviderDefinition,
    kind: ProviderKind,
}

const PROBER_SECONDARY: &[OperationKind] = &[OperationKind::HashFile];
const TRANSCODER_SECONDARY: &[OperationKind] =
    &[OperationKind::ExtractAudio, OperationKind::TranscribeAudio];
const BACKUP_SECONDARY: &[OperationKind] = &[OperationKind::DeleteArtifact];

const PROVIDERS: &[ProviderCatalogEntry] = &[
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-scanner",
            provider: "fake-scanner",
            primary: OperationKind::ScanLibrary,
            secondary: &[],
        },
        kind: ProviderKind::Scanner,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-prober",
            provider: "fake-prober",
            primary: OperationKind::ProbeFile,
            secondary: PROBER_SECONDARY,
        },
        kind: ProviderKind::Prober,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-transcoder",
            provider: "fake-transcoder",
            primary: OperationKind::TranscodeVideo,
            secondary: TRANSCODER_SECONDARY,
        },
        kind: ProviderKind::Transcoder,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-remuxer",
            provider: "fake-remuxer",
            primary: OperationKind::Remux,
            secondary: &[],
        },
        kind: ProviderKind::Remuxer,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-backup-store",
            provider: "fake-backup-store",
            primary: OperationKind::BackUpFile,
            secondary: BACKUP_SECONDARY,
        },
        kind: ProviderKind::BackupStore,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-health-checker",
            provider: "fake-health-checker",
            primary: OperationKind::VerifyArtifact,
            secondary: &[],
        },
        kind: ProviderKind::HealthChecker,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-identity-provider",
            provider: "fake-identity-provider",
            primary: OperationKind::IdentifyMedia,
            secondary: &[],
        },
        kind: ProviderKind::IdentityProvider,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-external-system",
            provider: "fake-external-system",
            primary: OperationKind::SyncExternalSystem,
            secondary: &[],
        },
        kind: ProviderKind::ExternalSystem,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-quality-scorer",
            provider: "fake-quality-scorer",
            primary: OperationKind::ScoreQuality,
            secondary: &[],
        },
        kind: ProviderKind::QualityScorer,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-issue-provider",
            provider: "fake-issue-provider",
            primary: OperationKind::CommitArtifact,
            secondary: &[],
        },
        kind: ProviderKind::IssueProvider,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-use-lease-provider",
            provider: "fake-use-lease-provider",
            primary: OperationKind::EditTracks,
            secondary: &[],
        },
        kind: ProviderKind::UseLeaseProvider,
    },
];

#[must_use]
pub fn provider_definition(binary_name: &str) -> Option<ProviderDefinition> {
    provider_entry(binary_name).map(|entry| entry.definition)
}

pub fn dispatch_provider(
    provider: &ProviderDefinition,
    req: &OperationRequest,
) -> Result<OperationDispatch, ProtocolError> {
    let entry =
        provider_entry(provider.binary_name).ok_or_else(|| ProtocolError::UnknownOperation {
            name: provider.binary_name.to_owned(),
        })?;
    if !supports_operation(&entry.definition, req.operation) {
        return Err(ProtocolError::UnknownOperation {
            name: operation_name(req.operation),
        });
    }

    let scenario = scenario(&req.payload);
    validate_payload(entry.kind, req)?;
    let timing = TimingControls::from_payload(&req.payload)?;
    let now = Utc::now();
    let response = OperationResponse {
        lease_id: req.lease_id,
        accepted_at: now,
    };
    let result_payload = result_payload(
        provider.provider,
        req.operation,
        scenario,
        &req.payload,
        timing.fan_out_count,
    )?;

    if timing.duration_ms == 0 {
        let progress = progress_frame(
            req.lease_id,
            0,
            now,
            PercentBps::ZERO,
            provider.provider,
            req.operation,
            scenario,
        );
        let result = ProgressFrame::Result {
            lease_id: req.lease_id,
            seq: 1,
            emitted_at: now,
            payload: result_payload,
        };
        return Ok(OperationDispatch::buffered(
            response,
            body_from_frames(&[progress, result])?,
        ));
    }

    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| invalid("timed fake dispatch requires tokio runtime"))?;
    let (writer, dispatch) = OperationDispatch::streaming(response);
    let timed = TimedDispatch {
        writer,
        lease_id: req.lease_id,
        provider: provider.provider.to_owned(),
        operation: req.operation,
        scenario: scenario.to_owned(),
        result_payload,
        duration_ms: timing.duration_ms,
        progress_interval_ms: timing.progress_interval_ms,
    };
    handle.spawn(async move {
        timed.emit().await;
    });
    Ok(dispatch)
}

pub async fn run_provider(binary_name: &'static str) -> Result<(), Box<dyn std::error::Error>> {
    let provider = provider_definition(binary_name)
        .ok_or_else(|| format!("unknown fake provider binary {binary_name}"))?;
    let credentials = load_credentials()?;
    let bind: SocketAddr = std::env::var("VOOM_WORKER_BIND")
        .unwrap_or_else(|_| "127.0.0.1:0".to_owned())
        .parse()
        .map_err(|e| format!("VOOM_WORKER_BIND parse failed: {e}"))?;
    let server = HttpServer::new(
        credentials,
        Arc::new(move |req| {
            let provider = provider;
            Box::pin(async move { dispatch_provider(&provider, &req) }) as OperationFuture
        }),
    );
    let running = server
        .serve(bind)
        .await
        .map_err(|e| format!("serve failed: {e}"))?;
    print_bound(running.bound);
    let shutdown_tx = running.shutdown;
    let joined = running.joined;
    let watchdog = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut bytes = Vec::new();
        let _ = stdin.read_to_end(&mut bytes).await;
        let _ = shutdown_tx.send(());
    });
    let _ = watchdog.await;
    let _ = joined.await;
    Ok(())
}

fn provider_entry(binary_name: &str) -> Option<ProviderCatalogEntry> {
    PROVIDERS
        .iter()
        .copied()
        .find(|entry| entry.definition.binary_name == binary_name)
}

fn supports_operation(provider: &ProviderDefinition, operation: OperationKind) -> bool {
    provider.primary == operation || provider.secondary.contains(&operation)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimingControls {
    duration_ms: u64,
    progress_interval_ms: u64,
    fan_out_count: Option<u32>,
}

impl TimingControls {
    fn from_payload(payload: &serde_json::Value) -> Result<Self, ProtocolError> {
        let duration_ms = optional_u64(payload, "duration_ms")?.unwrap_or(0);
        let progress_interval_ms =
            optional_u64(payload, "progress_interval_ms")?.unwrap_or(duration_ms);
        let fan_out_count = optional_u64(payload, "fan_out_count")?
            .map(u32::try_from)
            .transpose()
            .map_err(|_| invalid("fan_out_count out of range"))?;

        if duration_ms > MAX_FAKE_DURATION_MS {
            return Err(invalid("duration_ms exceeds fake-provider cap"));
        }
        if progress_interval_ms == 0 && duration_ms > 0 {
            return Err(invalid(
                "progress_interval_ms must be positive for timed runs",
            ));
        }

        Ok(Self {
            duration_ms,
            progress_interval_ms,
            fan_out_count,
        })
    }
}

struct TimedDispatch {
    writer: voom_worker_protocol::http::StreamingFrameWriter,
    lease_id: voom_core::LeaseId,
    provider: String,
    operation: OperationKind,
    scenario: String,
    result_payload: serde_json::Value,
    duration_ms: u64,
    progress_interval_ms: u64,
}

impl TimedDispatch {
    async fn emit(mut self) {
        let mut seq = 0_u64;
        let mut elapsed_ms = 0_u64;
        while elapsed_ms < self.duration_ms {
            let percent = percent_for(elapsed_ms, self.duration_ms);
            let frame = progress_frame(
                self.lease_id,
                seq,
                Utc::now(),
                percent,
                &self.provider,
                self.operation,
                &self.scenario,
            );
            if self.writer.write_frame(frame).await.is_err() {
                return;
            }
            seq += 1;

            let remaining_ms = self.duration_ms - elapsed_ms;
            let sleep_ms = self.progress_interval_ms.min(remaining_ms);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            elapsed_ms += sleep_ms;
        }

        let result = ProgressFrame::Result {
            lease_id: self.lease_id,
            seq,
            emitted_at: Utc::now(),
            payload: self.result_payload,
        };
        if self.writer.write_frame(result).await.is_ok() {
            let _ = self.writer.finish().await;
        }
    }
}

fn validate_payload(kind: ProviderKind, req: &OperationRequest) -> Result<(), ProtocolError> {
    match kind {
        ProviderKind::Scanner => {
            require_field(&req.payload, "path", "/library")?;
        }
        ProviderKind::Prober
        | ProviderKind::BackupStore
        | ProviderKind::HealthChecker
        | ProviderKind::IdentityProvider => {
            require_path(&req.payload)?;
        }
        ProviderKind::Transcoder => {
            require_path(&req.payload)?;
            require_field(&req.payload, "target_codec", "h265")?;
        }
        ProviderKind::Remuxer => {
            require_path(&req.payload)?;
            require_field(&req.payload, "container", "mkv")?;
        }
        ProviderKind::ExternalSystem => {
            require_path(&req.payload)?;
            require_field(&req.payload, "system", "plex")?;
            require_field(&req.payload, "action", "refresh")?;
        }
        ProviderKind::QualityScorer => {
            require_path(&req.payload)?;
            require_field(&req.payload, "profile", "default")?;
        }
        ProviderKind::IssueProvider => {
            require_path(&req.payload)?;
            require_field(&req.payload, "reason", "quality_regression")?;
        }
        ProviderKind::UseLeaseProvider => {
            require_path(&req.payload)?;
            require_field(&req.payload, "holder", "manual")?;
            require_field(&req.payload, "reason", "playback")?;
        }
    }
    Ok(())
}

fn require_path(payload: &serde_json::Value) -> Result<&str, ProtocolError> {
    let path = string_field(payload, "path")?;
    if path.trim().is_empty() {
        return Err(invalid("path must not be empty"));
    }
    Ok(path)
}

fn require_field(
    payload: &serde_json::Value,
    field: &'static str,
    expected: &'static str,
) -> Result<(), ProtocolError> {
    let actual = string_field(payload, field)?;
    if actual == expected {
        Ok(())
    } else {
        Err(invalid(format!("{field} must be {expected}")))
    }
}

fn string_field<'a>(
    payload: &'a serde_json::Value,
    field: &'static str,
) -> Result<&'a str, ProtocolError> {
    payload
        .as_object()
        .and_then(|object| object.get(field))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(format!("payload missing {field}")))
}

fn scenario(payload: &serde_json::Value) -> &str {
    payload
        .as_object()
        .and_then(|object| object.get("scenario"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("default")
}

fn result_payload(
    provider: &str,
    operation: OperationKind,
    scenario: &str,
    payload: &serde_json::Value,
    fan_out_count: Option<u32>,
) -> Result<serde_json::Value, ProtocolError> {
    let mut result = serde_json::json!({
        "provider": provider,
        "operation": operation_name(operation),
        "scenario": scenario,
    });
    let object = result
        .as_object_mut()
        .ok_or_else(|| invalid("internal result payload must be object"))?;
    match provider {
        "fake-scanner" => {
            object.insert("files".to_owned(), scanner_files(payload, fan_out_count)?);
        }
        "fake-prober" => {
            object.insert("duration_ms".to_owned(), serde_json::json!(7_200_000_u64));
            object.insert("codec".to_owned(), serde_json::json!("h264"));
            object.insert("hash".to_owned(), serde_json::json!("sha256:fake-prober"));
        }
        "fake-transcoder" => {
            object.insert(
                "output_path".to_owned(),
                serde_json::json!(transform_output_path(payload, "h265")),
            );
            object.insert(
                "target_codec".to_owned(),
                payload
                    .get("target_codec")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!("h265")),
            );
        }
        "fake-remuxer" => {
            object.insert(
                "output_path".to_owned(),
                serde_json::json!(transform_output_path(payload, "remuxed")),
            );
            object.insert(
                "container".to_owned(),
                payload
                    .get("container")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!("mkv")),
            );
        }
        "fake-backup-store" => {
            object.insert(
                "local_backup_id".to_owned(),
                serde_json::json!("backup-local-0001"),
            );
        }
        "fake-health-checker" => {
            object.insert("status".to_owned(), serde_json::json!("verified"));
        }
        "fake-identity-provider" => {
            object.insert(
                "canonical_media_id".to_owned(),
                serde_json::json!("media:fake:movie"),
            );
        }
        "fake-external-system" => {
            object.insert("refresh_status".to_owned(), serde_json::json!("queued"));
        }
        "fake-quality-scorer" => {
            object.insert("score".to_owned(), serde_json::json!(93));
            object.insert(
                "needs_transcode".to_owned(),
                serde_json::json!(needs_transcode(payload)),
            );
        }
        "fake-issue-provider" => {
            object.insert("issue_key".to_owned(), serde_json::json!("VOOM-FAKE-1"));
        }
        "fake-use-lease-provider" => {
            object.insert("decision".to_owned(), serde_json::json!("granted"));
        }
        _ => return Err(invalid(format!("unknown provider {provider}"))),
    }
    Ok(result)
}

fn optional_u64(
    payload: &serde_json::Value,
    field: &'static str,
) -> Result<Option<u64>, ProtocolError> {
    match payload.as_object().and_then(|object| object.get(field)) {
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid(format!("{field} must be an unsigned integer"))),
        None => Ok(None),
    }
}

fn progress_frame(
    lease_id: voom_core::LeaseId,
    seq: u64,
    emitted_at: chrono::DateTime<Utc>,
    percent: PercentBps,
    provider: &str,
    operation: OperationKind,
    scenario: &str,
) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq,
        emitted_at,
        percent: Some(percent),
        message: Some(format!(
            "{} handling {}",
            provider,
            operation_name(operation)
        )),
        payload: Some(serde_json::json!({
            "provider": provider,
            "operation": operation_name(operation),
            "scenario": scenario,
        })),
    }
}

fn percent_for(elapsed_ms: u64, duration_ms: u64) -> PercentBps {
    if duration_ms == 0 {
        return PercentBps::FULL;
    }
    let bps = elapsed_ms.saturating_mul(10_000) / duration_ms;
    PercentBps::try_from(u16::try_from(bps).unwrap_or(10_000)).unwrap_or(PercentBps::FULL)
}

fn scanner_files(
    payload: &serde_json::Value,
    fan_out_count: Option<u32>,
) -> Result<serde_json::Value, ProtocolError> {
    let base = string_field(payload, "path")?.trim_end_matches('/');
    let count = fan_out_count.unwrap_or(1);
    let files = (0..count)
        .map(|index| {
            serde_json::json!({
                "path": format!("{base}/file-{index:03}.mkv"),
                "size_bytes": 4_200_000_000_u64 + u64::from(index),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::Value::Array(files))
}

fn needs_transcode(payload: &serde_json::Value) -> bool {
    payload
        .get("codec")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|codec| codec != "h265")
}

fn transform_output_path(payload: &serde_json::Value, marker: &str) -> String {
    let path = payload
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("/library/movie.mkv");
    let input = Path::new(path);
    let parent = input
        .parent()
        .and_then(Path::to_str)
        .filter(|parent| !parent.is_empty())
        .unwrap_or("/library");
    let stem = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("movie");
    if parent == "/" {
        format!("/{stem}.{marker}.mkv")
    } else {
        format!("{}/{stem}.{marker}.mkv", parent.trim_end_matches('/'))
    }
}

fn body_from_frames(frames: &[ProgressFrame]) -> Result<Vec<u8>, ProtocolError> {
    let mut body = Vec::new();
    for frame in frames {
        let line = serde_json::to_vec(frame).map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("frame encode: {e}"),
        })?;
        body.extend_from_slice(&line);
        body.push(b'\n');
    }
    Ok(body)
}

fn operation_name(operation: OperationKind) -> String {
    serde_json::to_value(operation)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{operation:?}"))
}

fn load_credentials() -> Result<WorkerCredentials, Box<dyn std::error::Error>> {
    let secret = std::env::var("VOOM_WORKER_SECRET").map_err(|_| "VOOM_WORKER_SECRET not set")?;
    let worker_id: u64 = std::env::var("VOOM_WORKER_ID")
        .map_err(|_| "VOOM_WORKER_ID not set")?
        .parse()
        .map_err(|_| "VOOM_WORKER_ID not parseable")?;
    let worker_epoch: u64 = std::env::var("VOOM_WORKER_EPOCH")
        .map_err(|_| "VOOM_WORKER_EPOCH not set")?
        .parse()
        .map_err(|_| "VOOM_WORKER_EPOCH not parseable")?;
    Ok(WorkerCredentials {
        worker_id: voom_core::WorkerId(worker_id),
        worker_epoch,
        secret: SecretString::from(secret),
    })
}

#[expect(
    clippy::print_stdout,
    reason = "fake providers advertise readiness with BOUND addr=..."
)]
fn print_bound(bound: SocketAddr) {
    println!("BOUND addr={bound}");
}

fn invalid(detail: impl Into<String>) -> ProtocolError {
    ProtocolError::InvalidPayload {
        detail: detail.into(),
    }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
