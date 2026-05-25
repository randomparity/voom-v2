# Issue 72 Sidecar Ingest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist matching `.srt` subtitle sidecars during directory scans and expose durable bundle links in scan/Chaos observed-state output.

**Architecture:** Keep scan discovery responsible for classifying media candidates and matched sidecar paths. Keep scan persistence responsible for one atomic per-primary transaction that writes primary identity, media snapshot, sidecar identity, provisional bundle rows, and events. Reuse existing identity and bundle repositories; add only the narrow bundle lookup helper needed for idempotent linking.

**Tech Stack:** Rust, tokio, sqlx, serde JSON CLI envelopes, sibling unit tests, insta CLI snapshots.

---

### Task 1: Discovery Model and Matching

**Files:**
- Modify: `crates/voom-control-plane/src/scan/discovery.rs`
- Modify: `crates/voom-control-plane/src/scan/discovery_test.rs`

- [ ] **Step 1: Add failing discovery tests**

Add tests to `discovery_test.rs`:

```rust
#[tokio::test]
async fn directory_discovery_attaches_matching_srt_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let media = write_file(dir.path(), "Movie.Name.mkv", b"media");
    let sidecar = write_file(dir.path(), "Movie.Name.eng.srt", b"subtitle");
    let exact = write_file(dir.path(), "Movie.Name.srt", b"subtitle");
    let other = write_file(dir.path(), "Other.eng.srt", b"subtitle");

    let scan = discover_path(dir.path()).await.unwrap();

    assert_eq!(scan.candidates.len(), 1);
    assert_eq!(scan.candidates[0].path, media);
    assert_eq!(
        scan.candidates[0]
            .sidecars
            .iter()
            .map(|sidecar| sidecar.path.as_path())
            .collect::<Vec<_>>(),
        vec![exact.as_path(), sidecar.as_path()]
    );
    assert_eq!(
        scan.skipped.iter().map(|file| file.path.as_path()).collect::<Vec<_>>(),
        vec![other.as_path()]
    );
}

#[tokio::test]
async fn directory_discovery_assigns_sidecar_to_longest_matching_media_stem() {
    let dir = tempfile::tempdir().unwrap();
    let shorter = write_file(dir.path(), "Movie.mkv", b"short");
    let longer = write_file(dir.path(), "Movie.Part1.mkv", b"long");
    let sidecar = write_file(dir.path(), "Movie.Part1.eng.srt", b"subtitle");

    let scan = discover_path(dir.path()).await.unwrap();

    let shorter = scan.candidates.iter().find(|candidate| candidate.path == shorter).unwrap();
    let longer = scan.candidates.iter().find(|candidate| candidate.path == longer).unwrap();
    assert!(shorter.sidecars.is_empty());
    assert_eq!(longer.sidecars[0].path, sidecar);
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p voom-control-plane scan::discovery::tests::directory_discovery_attaches_matching_srt_sidecars scan::discovery::tests::directory_discovery_assigns_sidecar_to_longest_matching_media_stem -- --nocapture
```

Use two separate commands if the local Cargo version treats the second filter
as a test-binary argument:

```bash
cargo test -p voom-control-plane scan::discovery::tests::directory_discovery_attaches_matching_srt_sidecars -- --nocapture
cargo test -p voom-control-plane scan::discovery::tests::directory_discovery_assigns_sidecar_to_longest_matching_media_stem -- --nocapture
```

Expected: compile failure because `ScanCandidate.sidecars` does not exist.

- [ ] **Step 3: Implement discovery structs and matching**

In `discovery.rs`, change `ScanCandidate` and add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCandidate {
    pub path: PathBuf,
    pub sidecars: Vec<SidecarCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarCandidate {
    pub path: PathBuf,
}

fn is_supported_sidecar_path(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("srt"))
}
```

Update file scan candidates to use `sidecars: Vec::new()`. In directory scan,
collect media paths, sidecar paths, and unsupported skipped paths separately.
After traversal, assign each sidecar to the best media candidate:

```rust
fn sidecar_matches_media(media: &Path, sidecar: &Path) -> Option<usize> {
    let media_stem = media.file_stem()?.to_str()?;
    let sidecar_stem = sidecar.file_stem()?.to_str()?;
    if sidecar_stem == media_stem {
        return Some(media_stem.len());
    }
    sidecar_stem
        .strip_prefix(media_stem)
        .filter(|suffix| suffix.starts_with('.'))
        .map(|_| media_stem.len())
}
```

Sort candidates by path and sidecars by path after assignment. Unmatched `.srt`
files become `SkippedUnsupportedExtension`.

- [ ] **Step 4: Run GREEN**

Run:

```bash
cargo test -p voom-control-plane scan::discovery -- --nocapture
```

Expected: all discovery tests pass.

### Task 2: Bundle Repo Lookup for Idempotent Linking

**Files:**
- Modify: `crates/voom-store/src/repo/bundles.rs`
- Modify: `crates/voom-store/src/repo/bundles_test.rs`

- [ ] **Step 1: Add failing repo test**

Add:

```rust
#[tokio::test]
async fn get_member_by_file_asset_in_tx_returns_existing_membership() {
    let (repo, _identity, mv_id, asset_id, _asset_b, _tmp) = fresh().await;
    let bundle = repo
        .create(NewAssetBundle {
            media_variant_id: mv_id,
            display_name: "primary".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
    repo.add_member(NewBundleMember {
        bundle_id: bundle.id,
        file_asset_id: asset_id,
        role: BundleMemberRole::PrimaryVideo,
    })
    .await
    .unwrap();

    let mut tx = repo.pool.begin().await.unwrap();
    let found = repo
        .get_member_by_file_asset_in_tx(&mut tx, asset_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(found.bundle_id, bundle.id);
    assert_eq!(found.file_asset_id, asset_id);
    assert_eq!(found.role, BundleMemberRole::PrimaryVideo);
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p voom-store repo::bundles::tests::get_member_by_file_asset_in_tx_returns_existing_membership -- --nocapture
```

Expected: compile failure because the trait method does not exist.

- [ ] **Step 3: Add lookup method**

Add to `BundleRepo`:

```rust
async fn get_member_by_file_asset_in_tx<'tx>(
    &self,
    tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
    file_asset_id: FileAssetId,
) -> Result<Option<BundleMember>, VoomError>;
```

Implement in `SqliteBundleRepo` with:

```rust
let row = sqlx::query(
    "SELECT id, bundle_id, file_asset_id, role FROM asset_bundle_members \
     WHERE file_asset_id = ?",
)
.bind(i64_from_u64(file_asset_id.0))
.fetch_optional(&mut **tx)
.await
.map_err(|e| VoomError::Database(format!("asset_bundle_members get_by_asset: {e}")))?;
row.as_ref().map(row_to_bundle_member).transpose()
```

- [ ] **Step 4: Run GREEN**

Run:

```bash
cargo test -p voom-store repo::bundles::tests::get_member_by_file_asset_in_tx_returns_existing_membership -- --nocapture
```

Expected: pass.

### Task 3: Atomic Sidecar Persistence

**Files:**
- Modify: `crates/voom-control-plane/src/scan/persist.rs`
- Modify: `crates/voom-control-plane/src/scan/persist_test.rs`
- Modify: `crates/voom-control-plane/src/scan/mod.rs`
- Modify: `crates/voom-control-plane/src/scan/mod_test.rs`

- [ ] **Step 1: Add scan persistence tests**

In `scan/mod_test.rs`, add:

```rust
#[tokio::test]
async fn directory_scan_persists_matching_srt_sidecar_as_bundle_member() {
    let dir = tempfile::tempdir().unwrap();
    let media = write_file(dir.path(), "Movie.Name.mkv", b"movie");
    let sidecar = write_file(dir.path(), "Movie.Name.eng.srt", b"subtitle");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::AllSuccess);

    let report = cp
        .scan_path_with_launcher(
            ScanPathInput { path: dir.path().to_path_buf() },
            &mut launcher,
        )
        .await
        .unwrap();

    assert_eq!(report.summary.ingested, 2);
    assert_eq!(report.summary.snapshots_recorded, 1);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].path, media);
    assert_eq!(report.files[0].bundle_member_role.as_deref(), Some("primary_video"));
    assert_eq!(report.files[0].sidecars.len(), 1);
    assert_eq!(report.files[0].sidecars[0].path, sidecar);
    assert_eq!(report.files[0].sidecars[0].bundle_member_role, "external_subtitle");
    assert!(report.files[0].sidecars[0].content_hash.starts_with("sha256:"));
    assert_eq!(table_count(&cp, "file_assets").await, 2);
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);
    assert_eq!(table_count(&cp, "media_works").await, 1);
    assert_eq!(table_count(&cp, "media_variants").await, 1);
    assert_eq!(table_count(&cp, "asset_bundles").await, 1);
    assert_eq!(table_count(&cp, "asset_bundle_members").await, 2);
}

#[tokio::test]
async fn repeated_directory_scan_reuses_existing_sidecar_bundle() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "Movie.Name.mkv", b"movie");
    write_file(dir.path(), "Movie.Name.eng.srt", b"subtitle");
    let (cp, _db) = cp_with_manual_clock(T0).await;

    cp.scan_path_with_launcher(
        ScanPathInput { path: dir.path().to_path_buf() },
        &mut FakeLauncher::new(FakePlan::AllSuccess),
    )
    .await
    .unwrap();
    cp.scan_path_with_launcher(
        ScanPathInput { path: dir.path().to_path_buf() },
        &mut FakeLauncher::new(FakePlan::AllSuccess),
    )
    .await
    .unwrap();

    assert_eq!(table_count(&cp, "asset_bundles").await, 1);
    assert_eq!(table_count(&cp, "asset_bundle_members").await, 2);
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p voom-control-plane scan::mod_test::directory_scan_persists_matching_srt_sidecar_as_bundle_member scan::mod_test::repeated_directory_scan_reuses_existing_sidecar_bundle -- --nocapture
```

Use two separate commands if needed:

```bash
cargo test -p voom-control-plane scan::mod_test::directory_scan_persists_matching_srt_sidecar_as_bundle_member -- --nocapture
cargo test -p voom-control-plane scan::mod_test::repeated_directory_scan_reuses_existing_sidecar_bundle -- --nocapture
```

Expected: compile failures for missing report fields.

- [ ] **Step 3: Extend report types**

In `scan/mod.rs`, add:

```rust
pub struct ScanSidecarReport {
    pub path: PathBuf,
    pub file_asset_id: FileAssetId,
    pub file_version_id: FileVersionId,
    pub file_location_id: FileLocationId,
    pub bundle_id: voom_core::BundleId,
    pub bundle_member_role: String,
    pub content_hash: String,
    pub size_bytes: u64,
}
```

Add to `ScanFileReport`:

```rust
pub bundle_id: Option<voom_core::BundleId>,
pub bundle_member_role: Option<String>,
pub sidecars: Vec<ScanSidecarReport>,
```

Initialize these fields to `None`/empty for skipped and failed rows.

- [ ] **Step 4: Implement persistence**

In `persist.rs`:

- add `SidecarToPersist { path: PathBuf }`;
- add `PersistedSidecar` matching `ScanSidecarReport`;
- add `sidecars: Vec<PersistedSidecar>`, `bundle_id`, and
  `bundle_member_role` to `PersistedScan`;
- change `persist_scanned_media_snapshot` to accept `sidecars: &[SidecarCandidate]`.

Move primary media identity, media snapshot, sidecar identity, provisional
media work/variant/bundle creation, member linking, and event append calls into
one transaction before commit. Reuse `emit_ingest_events` for sidecar
`record_discovered_file_in_tx` outcomes.

For sidecar hashing, add:

```rust
async fn observe_sidecar(path: &Path) -> Result<(u64, String), VoomError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| VoomError::Config(format!("sidecar read {}: {e}", path.display())))?;
    let size_bytes = u64::try_from(bytes.len())
        .map_err(|_| VoomError::Internal(format!("sidecar too large: {}", path.display())))?;
    let hash = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
    Ok((size_bytes, hash))
}
```

Import `sha2::Digest as _`. If `sha2` is not already a dependency of
`voom-control-plane`, add it from workspace dependencies in `Cargo.toml`.

For provisional bundle creation, use:

```rust
NewMediaWork {
    kind: MediaWorkKind::Unknown,
    display_title: display_name_from_path(canonical_path),
    provisional: true,
    created_at: now,
}
NewMediaVariant {
    media_work_id: work.id,
    label: "scan".to_owned(),
    provisional: true,
    created_at: now,
}
NewAssetBundle {
    media_variant_id: variant.id,
    display_name: display_name_from_path(canonical_path),
    created_at: now,
}
```

Append the matching `MediaWorkCreated`, `MediaVariantCreated`,
`AssetBundleCreated`, and `AssetBundleMemberAdded` events in the same
transaction.

- [ ] **Step 5: Wire scan orchestration**

In `scan/mod.rs`, pass `&candidate.sidecars` into
`persist_scanned_media_snapshot`. Update `scanned_file_report` to copy
`bundle_id`, `bundle_member_role`, and sidecars from `PersistedScan`.

- [ ] **Step 6: Run GREEN**

Run:

```bash
cargo test -p voom-control-plane scan::mod_test::directory_scan_persists_matching_srt_sidecar_as_bundle_member scan::mod_test::repeated_directory_scan_reuses_existing_sidecar_bundle -- --nocapture
```

Use two separate commands if needed:

```bash
cargo test -p voom-control-plane scan::mod_test::directory_scan_persists_matching_srt_sidecar_as_bundle_member -- --nocapture
cargo test -p voom-control-plane scan::mod_test::repeated_directory_scan_reuses_existing_sidecar_bundle -- --nocapture
```

Expected: pass.

### Task 4: CLI Envelope and Observed-State Export

**Files:**
- Modify: `crates/voom-cli/src/commands/scan.rs`
- Modify: `crates/voom-cli/src/commands/scan_test.rs`
- Modify: `crates/voom-cli/tests/scan_envelope.rs`
- Modify: `crates/voom-cli/tests/support/observed_state.rs`
- Modify: `crates/voom-cli/tests/chaos_librarian_e2e.rs` if needed for stronger assertions.

- [ ] **Step 1: Add CLI integration test**

Add to `scan_envelope.rs`:

```rust
#[tokio::test]
async fn scan_directory_outputs_durable_sidecar_links() {
    let seeded = seed().await;
    let dir = TempDir::new().unwrap();
    let media = dir.path().join("Movie.Name.mp4");
    std::fs::copy(tiny_media_fixture(), &media).unwrap();
    let sidecar = dir.path().join("Movie.Name.eng.srt");
    std::fs::write(&sidecar, b"1\n00:00:00,000 --> 00:00:01,000\nHello\n").unwrap();

    let output = scan_command(&seeded.url, dir.path()).output().unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    let file = &json["data"]["files"][0];
    assert_eq!(file["bundle_member_role"], "primary_video");
    assert!(file["bundle_id"].as_u64().unwrap() > 0);
    assert_eq!(file["sidecars"].as_array().unwrap().len(), 1);
    assert_eq!(file["sidecars"][0]["bundle_member_role"], "external_subtitle");
    assert!(file["sidecars"][0]["content_hash"].as_str().unwrap().starts_with("sha256:"));

    let pool = voom_store::connect(&seeded.url).await.unwrap();
    assert_table_count(&pool, "file_assets", 2).await;
    assert_table_count(&pool, "asset_bundle_members", 2).await;
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p voom-cli --test scan_envelope scan_directory_outputs_durable_sidecar_links -- --nocapture
```

Expected: compile failure or missing JSON fields.

- [ ] **Step 3: Extend CLI data structs**

In `commands/scan.rs`, add:

```rust
#[derive(Debug, Serialize)]
pub struct ScanSidecarData {
    pub path: String,
    pub file_asset_id: u64,
    pub file_version_id: u64,
    pub file_location_id: u64,
    pub bundle_id: u64,
    pub bundle_member_role: String,
    pub content_hash: String,
    pub size_bytes: u64,
}
```

Add optional `bundle_id`, optional `bundle_member_role`, and `sidecars` to
`ScanFileData`. Convert from `ScanSidecarReport` using `path_wire`.

- [ ] **Step 4: Replace observed-state filesystem heuristic**

In `observed_state.rs`, change the asset query to include bundle membership for
primary rows and exclude rows whose only role is `external_subtitle`, so durable
sidecar assets do not also appear as top-level observed assets:

```sql
SELECT fa.id AS file_asset_id, fv.id AS file_version_id, fv.content_hash,
       fv.size_bytes, fl.value AS location_value, ms.payload AS snapshot_payload,
       bm.bundle_id AS bundle_id
FROM file_assets fa
JOIN file_versions fv ON fv.file_asset_id = fa.id AND fv.retired_at IS NULL
JOIN file_locations fl ON fl.file_version_id = fv.id
    AND fl.retired_at IS NULL AND fl.kind = 'local_path'
LEFT JOIN asset_bundle_members bm ON bm.file_asset_id = fa.id
LEFT JOIN media_snapshots ms ON ms.id = (
    SELECT max(ms2.id) FROM media_snapshots ms2 WHERE ms2.file_version_id = fv.id
)
WHERE fa.retired_at IS NULL
  AND (bm.role IS NULL OR bm.role <> 'external_subtitle')
ORDER BY fa.id ASC, fv.id ASC, fl.id ASC
```

Add a second query keyed by bundle id that loads `external_subtitle` members:

```sql
SELECT bm.bundle_id, fa.id, fv.id, fv.content_hash, fv.size_bytes, fl.value
FROM asset_bundle_members bm
JOIN file_assets fa ON fa.id = bm.file_asset_id AND fa.retired_at IS NULL
JOIN file_versions fv ON fv.file_asset_id = fa.id AND fv.retired_at IS NULL
JOIN file_locations fl ON fl.file_version_id = fv.id
    AND fl.retired_at IS NULL AND fl.kind = 'local_path'
WHERE bm.role = 'external_subtitle'
ORDER BY bm.bundle_id ASC, fl.value ASC
```

Group by bundle id, and for a primary asset row with `bundle_id`, emit
sidecars from the grouped durable rows. Delete `observed_sidecars`,
`collect_sidecar_candidates`, and `sha256_file`.

- [ ] **Step 5: Run GREEN**

Run:

```bash
cargo test -p voom-cli --test scan_envelope scan_directory_outputs_durable_sidecar_links -- --nocapture
cargo test -p voom-cli --test chaos_librarian_e2e static_library_baseline_scans_exports_and_compares -- --ignored --nocapture
```

Expected: pass.

### Task 5: Final Reviews and Verification

**Files:**
- Validate all changed files.

- [ ] **Step 1: Run adversarial code review**

Review the working-tree diff for correctness risks. Address material findings.
Do not run more than three adversarial reviews for the code-writing cycle.

- [ ] **Step 2: Run simplification review**

Review the working-tree diff for safe simplification. Address the highest-value
recommendations that do not broaden scope.

- [ ] **Step 3: Run focused verification**

Run:

```bash
cargo test -p voom-control-plane scan::discovery -- --nocapture
cargo test -p voom-control-plane scan::mod_test::directory_scan_persists_matching_srt_sidecar_as_bundle_member scan::mod_test::repeated_directory_scan_reuses_existing_sidecar_bundle -- --nocapture
cargo test -p voom-cli --test scan_envelope scan_directory_outputs_durable_sidecar_links -- --nocapture
cargo test -p voom-cli --test chaos_librarian_e2e static_library_baseline_scans_exports_and_compares -- --ignored --nocapture
git diff --check
```

Use separate commands for the two `scan::mod_test` filters if needed.

Expected: all commands pass.

- [ ] **Step 4: Run full CI**

Run:

```bash
just ci
```

Expected: all checks pass.

- [ ] **Step 5: Commit implementation**

Run:

```bash
git status --short
git add crates/voom-control-plane/src/scan/discovery.rs \
  crates/voom-control-plane/src/scan/discovery_test.rs \
  crates/voom-store/src/repo/bundles.rs \
  crates/voom-store/src/repo/bundles_test.rs \
  crates/voom-control-plane/src/scan/persist.rs \
  crates/voom-control-plane/src/scan/persist_test.rs \
  crates/voom-control-plane/src/scan/mod.rs \
  crates/voom-control-plane/src/scan/mod_test.rs \
  crates/voom-cli/src/commands/scan.rs \
  crates/voom-cli/src/commands/scan_test.rs \
  crates/voom-cli/tests/scan_envelope.rs \
  crates/voom-cli/tests/support/observed_state.rs \
  Cargo.toml
git commit -m "feat: ingest subtitle sidecars during scan"
```

Expected: commit succeeds after hooks pass.
