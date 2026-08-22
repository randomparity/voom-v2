//! Scan-flow seeding for integration tests (ADR 0077).
//!
//! The control-plane no longer reads bytes: discovery, hashing, and probing
//! belong to owner-node workers. Flow tests that previously seeded identity
//! rows through `scan_library_root` now drive the real durable path —
//! request → start → one evidence batch → complete — with canned probe
//! snapshots, and publication happens inside the completion transaction
//! exactly as it does in production.

use serde_json::Value as JsonValue;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use voom_control_plane::ControlPlane;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::scan::{
    RemoteScanBatchInput, RemoteScanCompleteInput, RemoteScanStartInput,
};
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::{
    ArtifactAccessMode, FileKeyFacts, MediaSnapshotId, NodeId, NodeIncarnationId, NodeKind,
    OperationKind, ProviderRelativeLocator, ScanObservationEvidence, ScanSessionId, StorageRootId,
};
use voom_store::repo::scan::sessions::ScanObservation;

/// One seeded source file: the durable ids the flows consume downstream.
#[derive(Debug, Clone, Copy)]
pub struct SeededSource {
    pub file_location_id: voom_core::FileLocationId,
    pub file_version_id: voom_core::FileVersionId,
    pub media_snapshot_id: MediaSnapshotId,
}

/// A file to seed: root-relative locator plus the probe snapshot to record.
#[derive(Debug, Clone)]
pub struct SeedFile<'a> {
    /// Root-relative `/`-joined locator; must match the fixture's name on disk.
    pub locator: &'a str,
    /// Absolute path of the fixture on disk (hashed for evidence).
    pub path: &'a Path,
    /// Normalized probe snapshot recorded verbatim into `media_snapshots`.
    pub probe_snapshot: JsonValue,
}

/// Distinguishes seeder nodes when one test seeds the same root repeatedly.
static SEEDER_SEQ: AtomicU64 = AtomicU64::new(0);

/// Seed `files` onto `root` through the real request/start/batch/complete
/// scan-session chain. Every file gets full agreeing evidence, so each one
/// publishes identity rows and a media snapshot.
///
/// # Panics
///
/// Panics when any session step or the final id lookup fails; flow fixtures
/// treat a seeding failure as the test failing loudly.
pub async fn seed_scanned_files(
    cp: &ControlPlane,
    db_url: &str,
    root: StorageRootId,
    files: &[SeedFile<'_>],
) -> Result<Vec<SeededSource>, Box<dyn std::error::Error>> {
    let call = SEEDER_SEQ.fetch_add(1, Ordering::Relaxed);
    let incarnation: NodeIncarnationId = format!("{call:032x}")
        .parse()
        .map_err(|error| format!("fixture incarnation parses: {error}"))?;
    let registered = cp
        .register_node(RegisterNodeInput {
            name: format!("flow-seeder-{root}-{call}"),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 600,
            metadata: serde_json::json!({}),
        })
        .await?;
    let node_id = registered.node.id;
    cp.remote_activate(RemoteActivateInput {
        node_id,
        token: registered.token.clone(),
        idempotency_key: format!("activate-flow-seeder-{root}-{call}"),
        request_hash: request_hash(&format!("activate-flow-seeder-{root}-{call}")),
        incarnation_id: incarnation,
        workers: vec![RemoteWorkerDeclaration {
            logical_name: "flow-seeder".to_owned(),
            operations: vec![OperationKind::ScanLibrary],
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            max_parallel: 1,
        }],
    })
    .await?;

    // The shared seeded test root (`seed_test_storage_root`) is owned by a
    // fixture node this helper cannot authenticate as, so ownership is
    // transferred to the freshly registered seeder for the duration of the
    // session and restored afterwards: downstream local-execution paths in the
    // flows assert the root belongs to the control plane's own node.
    let previous_owner_node_id: i64 = {
        let pool = voom_store::connect(db_url).await?;
        sqlx::query_scalar("SELECT owner_node_id FROM library_roots WHERE id = ?")
            .bind(i64::try_from(root.0).map_err(|error| format!("root id fits i64: {error}"))?)
            .fetch_one(&pool)
            .await
            .map_err(|error| format!("read test root owner: {error}"))?
    };
    claim_test_root(db_url, root, node_id.0).await?;

    let outcome = cp.request_scan_run(root, 600).await?;
    let voom_control_plane::scan::ScanRunOutcome::Requested(requested) = outcome else {
        return Err(format!("seed root {root} must request").into());
    };
    let session = requested.scan_session_id;

    let token = registered.token;
    start_session(cp, node_id, session, incarnation, &token).await?;
    submit_batch(cp, node_id, session, incarnation, &token, files).await?;
    complete_session(cp, node_id, session, incarnation, &token, files.len()).await?;
    claim_test_root(
        db_url,
        root,
        u64::try_from(previous_owner_node_id)
            .map_err(|error| format!("previous owner fits u64: {error}"))?,
    )
    .await?;

    read_published_ids(db_url, files).await
}

/// Point the test root's `owner_node_id` at `node_id` so `request_scan_run`
/// creates a session this seeder's credentials may drive.
async fn claim_test_root(
    db_url: &str,
    root: StorageRootId,
    node_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = voom_store::connect(db_url).await?;
    let claimed = sqlx::query(
        "UPDATE library_roots SET owner_node_id = ? \
         WHERE id = ? AND state = 'active'",
    )
    .bind(i64::try_from(node_id).map_err(|error| format!("node id fits i64: {error}"))?)
    .bind(i64::try_from(root.0).map_err(|error| format!("root id fits i64: {error}"))?)
    .execute(&pool)
    .await
    .map_err(|error| format!("claim test root {root}: {error}"))?;
    if claimed.rows_affected() != 1 {
        return Err(format!("active test root {root} must exist to seed").into());
    }
    Ok(())
}

async fn start_session(
    cp: &ControlPlane,
    node_id: NodeId,
    session: ScanSessionId,
    incarnation: NodeIncarnationId,
    token: &secrecy::SecretString,
) -> Result<(), Box<dyn std::error::Error>> {
    cp.start_scan_session(RemoteScanStartInput {
        node_id,
        scan_session_id: session,
        incarnation_id: incarnation,
        token: token.clone(),
        idempotency_key: format!("flow-seed-start-{session}"),
        request_hash: request_hash(&format!("flow-seed-start-{session}")),
    })
    .await?;
    Ok(())
}

async fn submit_batch(
    cp: &ControlPlane,
    node_id: NodeId,
    session: ScanSessionId,
    incarnation: NodeIncarnationId,
    token: &secrecy::SecretString,
    files: &[SeedFile<'_>],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut observations = Vec::with_capacity(files.len());
    for file in files {
        let bytes = std::fs::read(file.path)?;
        let digest = blake3_hash(&bytes);
        let metadata = std::fs::metadata(file.path)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(|| "1970-01-01T00:00:00Z".to_owned(), rfc3339);
        #[cfg(unix)]
        let file_key = {
            use std::os::unix::fs::MetadataExt;
            Some(FileKeyFacts {
                dev: metadata.dev(),
                ino: metadata.ino(),
                nlink: metadata.nlink(),
            })
        };
        #[cfg(not(unix))]
        let file_key = None;
        observations.push(ScanObservation {
            provider_relative_locator: ProviderRelativeLocator::new(file.locator.to_owned())
                .map_err(|error| format!("fixture locator {}: {error}", file.locator))?,
            provider_object_identity: format!(
                "dev={};ino={}",
                file_key.as_ref().map_or(0, |key| key.dev),
                file_key.as_ref().map_or(0, |key| key.ino)
            ),
            size_bytes: u64::try_from(bytes.len())
                .map_err(|error| format!("fixture size fits u64: {error}"))?,
            modified_at: parse_time(&modified)?,
            stability_started_at: parse_time(&modified)?,
            stability_confirmed_at: parse_time(&modified)?,
            evidence: Some(ScanObservationEvidence {
                content_hash: format!("blake3:{digest}"),
                size_bytes: u64::try_from(bytes.len())
                    .map_err(|error| format!("fixture size fits u64: {error}"))?,
                modified_at: modified.clone(),
                file_key,
                sidecars: Vec::new(),
                probe_snapshot: file.probe_snapshot.clone(),
            }),
        });
    }
    cp.accept_scan_observation_batch(RemoteScanBatchInput {
        node_id,
        scan_session_id: session,
        incarnation_id: incarnation,
        token: token.clone(),
        idempotency_key: format!("flow-seed-batch-{session}-0"),
        request_hash: request_hash(&format!("flow-seed-batch-{session}-0")),
        sequence: 0,
        observations,
    })
    .await?;
    Ok(())
}

async fn complete_session(
    cp: &ControlPlane,
    node_id: NodeId,
    session: ScanSessionId,
    incarnation: NodeIncarnationId,
    token: &secrecy::SecretString,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if count == 0 {
        return Ok(());
    }
    let outcome = cp
        .complete_scan_session(RemoteScanCompleteInput {
            node_id,
            scan_session_id: session,
            incarnation_id: incarnation,
            token: token.clone(),
            idempotency_key: format!("flow-seed-complete-{session}"),
            request_hash: request_hash(&format!("flow-seed-complete-{session}")),
            last_sequence: Some(0),
            observation_count: u64::try_from(count)
                .map_err(|error| format!("count fits u64: {error}"))?,
        })
        .await?;
    if outcome.status != voom_core::ScanSessionStatus::Succeeded {
        return Err(format!(
            "seeded session ended {:?}, expected succeeded",
            outcome.status
        )
        .into());
    }
    Ok(())
}

async fn read_published_ids(
    db_url: &str,
    files: &[SeedFile<'_>],
) -> Result<Vec<SeededSource>, Box<dyn std::error::Error>> {
    let pool = voom_store::connect(db_url).await?;
    let mut seeded = Vec::with_capacity(files.len());
    for file in files {
        let row: (i64, i64, i64) = sqlx::query_as(
            "SELECT fl.id, fv.id, ms.id \
             FROM file_locations fl \
             JOIN file_versions fv ON fv.id = fl.file_version_id \
             JOIN media_snapshots ms ON ms.file_version_id = fv.id \
             WHERE fl.provider_relative_locator = ? \
             ORDER BY fl.id DESC LIMIT 1",
        )
        .bind(file.locator)
        .fetch_one(&pool)
        .await
        .map_err(|error| format!("published rows for {}: {error}", file.locator))?;
        let [location, version, snapshot] = [row.0, row.1, row.2];
        let positive = |value: i64| {
            u64::try_from(value).map_err(|error| format!("published id fits u64: {error}"))
        };
        seeded.push(SeededSource {
            file_location_id: voom_core::FileLocationId(positive(location)?),
            file_version_id: voom_core::FileVersionId(positive(version)?),
            media_snapshot_id: MediaSnapshotId(positive(snapshot)?),
        });
    }
    Ok(seeded)
}

/// Route-level `request_hash` inputs must be lowercase SHA-256-shaped; any
/// stable 64-char lowercase-hex digest satisfies the format gate.
fn request_hash(label: &str) -> String {
    blake3_hash(label.as_bytes())
}
fn blake3_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn rfc3339(from_epoch: std::time::Duration) -> String {
    let seconds = i64::try_from(from_epoch.as_secs()).unwrap_or(0);
    let nanos = from_epoch.subsec_nanos();
    // `time` is not a dependency here; emit a coarse RFC 3339 stamp. The
    // publication stores it verbatim; no flow asserts wall-clock equality.
    let (year, month, day, hour, minute, second) = civil_from_unix(seconds);
    if nanos == 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{nanos:09}Z")
    }
}

/// Days-from-civil inverse (Howard Hinnant's algorithm) — enough precision for
/// fixture stamps.
fn civil_from_unix(seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
        u32::try_from(secs_of_day / 3600).unwrap_or(0),
        u32::try_from((secs_of_day % 3600) / 60).unwrap_or(0),
        u32::try_from(secs_of_day % 60).unwrap_or(0),
    )
}

fn parse_time(stamp: &str) -> Result<time::OffsetDateTime, Box<dyn std::error::Error>> {
    time::OffsetDateTime::parse(stamp, &time::format_description::well_known::Rfc3339)
        .map_err(|error| format!("fixture timestamp {stamp} parses: {error}").into())
}
