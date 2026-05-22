# VOOM Sprint 3 Policy Inputs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Sprint 3 policy input model, deterministic fixtures, SQLite persistence, repository API, and control-plane use cases described in `docs/superpowers/specs/2026-05-22-voom-sprint-3-design.md`.

**Architecture:** `voom-policy` owns serde-enabled domain types and fixture loading with no database dependency. `voom-store` owns migration 0006 plus `PolicyInputRepo`, using database-enforced synthetic target integrity. `voom-control-plane` composes transactional create/get/list use cases without adding CLI commands or event vocabulary.

**Tech Stack:** Rust 2024, serde/serde_json, time, sqlx SQLite, tokio tests, existing sibling unit-test layout, `just ci`.

---

## File Map

- `crates/voom-policy/Cargo.toml`: add `serde`, `serde_json`, `time`, and `voom-core` dependencies.
- `crates/voom-policy/src/lib.rs`: replace the reserved empty module with domain exports.
- `crates/voom-policy/src/model.rs` and `model_test.rs`: domain types and validation tests.
- `crates/voom-policy/src/fixtures.rs` and `fixtures_test.rs`: embedded fixture loader and round-trip tests.
- `crates/voom-policy/fixtures/*.json`: required compliant and noncompliant fixtures.
- `migrations/0006_policy_inputs.sql`: policy input persistence schema.
- `crates/voom-store/src/migrator.rs`: register migration 0006.
- `crates/voom-store/src/repo/policy_inputs.rs` and `policy_inputs_test.rs`: repository trait, SQLite implementation, DB round-trip and raw SQL constraint tests.
- `crates/voom-store/src/repo/mod.rs`: export the new repo.
- `crates/voom-store/Cargo.toml`: add `voom-policy` dependency.
- `crates/voom-control-plane/src/lib.rs`: add `SqlitePolicyInputRepo` field and accessor.
- `crates/voom-control-plane/src/cases/policy_inputs.rs` and `policy_inputs_test.rs`: control-plane use cases and tests.
- `crates/voom-control-plane/src/cases/mod.rs`: expose the new case module.
- `crates/voom-control-plane/Cargo.toml`: add `voom-policy` dependency if tests or signatures need public model types directly.

## Task 1: `voom-policy` Domain Model

**Files:**
- Modify: `crates/voom-policy/Cargo.toml`
- Modify: `crates/voom-policy/src/lib.rs`
- Create: `crates/voom-policy/src/model.rs`
- Create: `crates/voom-policy/src/model_test.rs`

- [ ] **Step 1: Add crate dependencies**

Add:

```toml
[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
time = { workspace = true, features = ["serde"] }
voom-core = { workspace = true }
```

- [ ] **Step 2: Replace the reserved exports**

Use this `lib.rs` shape:

```rust
//! Policy-domain inputs for Sprint 3.

pub mod fixtures;
pub mod model;

pub use fixtures::{FixtureName, load_fixture};
pub use model::{
    BundleTargetInput, BundleTargetState, IssueInput, IssueInputState, MediaSnapshotInput,
    PolicyInputSetDraft, PolicyInputSetValidationError, PolicyInputSourceKind,
    PolicySyntheticTarget, QualityProfileSelection, TargetKind, TargetRef,
    IdentityEvidenceInput, validate_input_set,
};
```

- [ ] **Step 3: Add model types**

Implement `model.rs` with these public enums and structs:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyInputSourceKind {
    Fixture,
    Test,
    Imported,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    MediaWork,
    MediaVariant,
    AssetBundle,
    FileAsset,
    FileVersion,
    FileLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TargetRef {
    MediaWork { id: voom_core::MediaWorkId },
    MediaVariant { id: voom_core::MediaVariantId },
    AssetBundle { id: voom_core::BundleId },
    FileAsset { id: voom_core::FileAssetId },
    FileVersion { id: voom_core::FileVersionId },
    FileLocation { id: voom_core::FileLocationId },
    Synthetic { key: String, kind: TargetKind },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PolicyInputSetDraft {
    pub slug: String,
    pub display_name: String,
    pub schema_version: u32,
    pub source_kind: PolicyInputSourceKind,
    pub created_at: time::OffsetDateTime,
    pub description: Option<String>,
    pub fixture_labels: Vec<String>,
    pub synthetic_targets: Vec<PolicySyntheticTarget>,
    pub media_snapshots: Vec<MediaSnapshotInput>,
    pub identity_evidence: Vec<IdentityEvidenceInput>,
    pub bundle_targets: Vec<BundleTargetInput>,
    pub quality_profiles: Vec<QualityProfileSelection>,
    pub issues: Vec<IssueInput>,
}
```

Add child structs with fields from the spec: each child has `ordinal: u32`, `target: TargetRef`, and the relevant payload fields (`container`, `stream_summary`, `provenance`, `artifact_expectation`, `dimension_weights`) as `serde_json::Value`.

- [ ] **Step 4: Add validation**

Implement:

```rust
pub fn validate_input_set(input: &PolicyInputSetDraft) -> Result<(), PolicyInputSetValidationError>
```

Validation must reject:

- empty or whitespace-only slug;
- empty fixture labels;
- duplicate fixture labels;
- input sets with no media snapshot and no bundle target;
- synthetic child references without a matching `PolicySyntheticTarget`;
- synthetic key reused with a different `TargetKind`;
- evidence confidence outside `0.0..=1.0`;
- empty provider names and empty quality profile names.

- [ ] **Step 5: Add focused model tests**

In `model_test.rs`, add tests named:

- `valid_minimal_input_set_passes`;
- `empty_slug_is_rejected`;
- `duplicate_fixture_label_is_rejected`;
- `input_set_without_snapshot_or_bundle_target_is_rejected`;
- `undeclared_synthetic_target_is_rejected`;
- `synthetic_key_reused_with_different_kind_is_rejected`;
- `evidence_confidence_out_of_range_is_rejected`;
- `empty_provider_and_profile_names_are_rejected`.

Run: `cargo test -p voom-policy --all-features`

Expected before implementation: compile failure or failing tests. Expected after implementation: all `voom-policy` tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-policy
git commit -m "feat: add policy input domain model"
```

## Task 2: Deterministic Fixtures

**Files:**
- Create: `crates/voom-policy/src/fixtures.rs`
- Create: `crates/voom-policy/src/fixtures_test.rs`
- Create: `crates/voom-policy/fixtures/synthetic_compliant_baseline.json`
- Create: `crates/voom-policy/fixtures/synthetic_noncompliant_transcode_needed.json`

- [ ] **Step 1: Add fixture loader API**

Implement:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixtureName {
    SyntheticCompliantBaseline,
    SyntheticNoncompliantTranscodeNeeded,
}

pub fn load_fixture(name: FixtureName) -> Result<PolicyInputSetDraft, serde_json::Error>
```

Use `include_str!("../fixtures/<file>.json")` and deserialize to `PolicyInputSetDraft`.

- [ ] **Step 2: Add fixture content**

The compliant fixture must include:

- slug `synthetic-compliant-baseline`;
- fixture label `synthetic_compliant_baseline`;
- synthetic targets for one work, variant, bundle, asset, version, and location;
- one media snapshot with `container: "mkv"`, `video_codec: "hevc"`, English audio, English subtitle, and no health flags;
- one identity evidence row with confidence `0.99`;
- bundle target rows for primary video and external subtitle;
- quality profile `balanced-home`;
- no issue inputs.

The noncompliant fixture must include:

- slug `synthetic-noncompliant-transcode-needed`;
- fixture label `synthetic_noncompliant_transcode_needed`;
- matching synthetic target declarations;
- one media snapshot with `container: "mp4"`, `video_codec: "h264"`, and a missing English subtitle fact;
- one identity evidence row with confidence `0.91`;
- one bundle target that marks the English subtitle as `required`;
- quality profile `balanced-home`;
- one issue input with kind `policy_noncompliant`, severity `medium`, priority `normal`, and state `open`.

- [ ] **Step 3: Add fixture tests**

In `fixtures_test.rs`, add:

- `compliant_fixture_loads_and_validates`;
- `noncompliant_fixture_loads_and_validates`;
- `fixtures_round_trip_through_pretty_json`;
- `fixture_labels_are_canonical`.

Run: `cargo test -p voom-policy --all-features`

Expected: policy model and fixture tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-policy
git commit -m "test: add deterministic policy input fixtures"
```

## Task 3: Migration 0006

**Files:**
- Create: `migrations/0006_policy_inputs.sql`
- Modify: `crates/voom-store/src/migrator.rs`
- Modify: `crates/voom-store/tests/migration_inventory.rs` only if the expected migration list is explicit in the current file

- [ ] **Step 1: Add schema**

Create these tables:

- `policy_input_sets`;
- `policy_input_set_fixture_labels`;
- `policy_input_synthetic_targets`;
- `policy_media_snapshot_inputs`;
- `policy_identity_evidence_inputs`;
- `policy_bundle_target_inputs`;
- `policy_quality_profile_selections`;
- `policy_issue_inputs`.

For `policy_input_synthetic_targets`, include:

```sql
id                  INTEGER PRIMARY KEY,
policy_input_set_id INTEGER NOT NULL REFERENCES policy_input_sets(id) ON DELETE CASCADE,
synthetic_key       TEXT NOT NULL,
target_kind         TEXT NOT NULL CHECK (target_kind IN ('media_work','media_variant','asset_bundle','file_asset','file_version','file_location')),
display_name        TEXT,
UNIQUE (policy_input_set_id, synthetic_key),
UNIQUE (policy_input_set_id, id)
```

For every child input table, include:

- `policy_input_set_id INTEGER NOT NULL REFERENCES policy_input_sets(id) ON DELETE CASCADE`;
- `ordinal INTEGER NOT NULL CHECK (ordinal >= 0)`;
- durable target id columns for allowed target kinds;
- `synthetic_target_id INTEGER`;
- composite FK `(policy_input_set_id, synthetic_target_id) REFERENCES policy_input_synthetic_targets(policy_input_set_id, id)`;
- a `CHECK` that exactly one durable target id or `synthetic_target_id` is non-null.

- [ ] **Step 2: Register migration**

Add migration 0006 to `crates/voom-store/src/migrator.rs` in ascending version order.

- [ ] **Step 3: Verify migration inventory**

Run: `cargo test -p voom-store --test migration_inventory --all-features`

Expected: migration inventory tests pass.

- [ ] **Step 4: Commit**

```bash
git add migrations/0006_policy_inputs.sql crates/voom-store/src/migrator.rs crates/voom-store/tests/migration_inventory.rs
git commit -m "feat: add policy input persistence schema"
```

## Task 4: Store Repository

**Files:**
- Modify: `crates/voom-store/Cargo.toml`
- Modify: `crates/voom-store/src/repo/mod.rs`
- Create: `crates/voom-store/src/repo/policy_inputs.rs`
- Create: `crates/voom-store/src/repo/policy_inputs_test.rs`

- [ ] **Step 1: Add dependency and exports**

Add `voom-policy = { workspace = true }` to `crates/voom-store/Cargo.toml`; add `voom-policy` to `[workspace.dependencies]` if it is not already present. Export `policy_inputs` from `repo/mod.rs`.

- [ ] **Step 2: Define repo surface**

Create:

```rust
#[async_trait::async_trait]
pub trait PolicyInputRepo: Repository {
    async fn create_input_set(
        &self,
        input: voom_policy::PolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError>;

    async fn create_input_set_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: voom_policy::PolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError>;

    async fn get_input_set(&self, id: PolicyInputSetId) -> Result<Option<PolicyInputSet>, VoomError>;
    async fn get_input_set_by_slug(&self, slug: &str) -> Result<Option<PolicyInputSet>, VoomError>;
    async fn list_input_sets(&self) -> Result<Vec<PolicyInputSetSummary>, VoomError>;
}
```

Define `PolicyInputSetId(pub u64)` locally in this repo unless a shared id is added to `voom-core`. Keep Sprint 3 scoped: do not add policy document/version ids.

- [ ] **Step 3: Implement SQLite round trip**

`SqlitePolicyInputRepo::create_input_set_in_tx` must call `voom_policy::validate_input_set`, insert the root, labels, synthetic targets, and all child rows in one transaction. `get_input_set` and `get_input_set_by_slug` must reconstruct a deterministic `PolicyInputSet` projection ordered by `ordinal`.

- [ ] **Step 4: Add repository tests**

Add tests:

- `create_get_and_list_policy_input_set`;
- `duplicate_slug_is_rejected`;
- `fixture_labels_are_globally_unique`;
- `create_rolls_back_when_child_insert_fails`;
- `raw_sql_rejects_undeclared_synthetic_target`;
- `raw_sql_rejects_mixed_durable_and_synthetic_target_shape`;
- `raw_sql_rejects_cross_input_set_synthetic_target`;
- `sqlite_round_trip_matches_fixture_projection`.

For raw SQL tests, bypass the repo with `sqlx::query(...)` so the failure proves SQLite constraints, not Rust validation.

- [ ] **Step 5: Verify store tests**

Run: `cargo test -p voom-store --all-features policy_inputs`

Expected: policy input repo tests pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/voom-store migrations
git commit -m "feat: add policy input repository"
```

## Task 5: Control-Plane Use Cases

**Files:**
- Modify: `crates/voom-control-plane/Cargo.toml`
- Modify: `crates/voom-control-plane/src/lib.rs`
- Modify: `crates/voom-control-plane/src/cases/mod.rs`
- Create: `crates/voom-control-plane/src/cases/policy_inputs.rs`
- Create: `crates/voom-control-plane/src/cases/policy_inputs_test.rs`

- [ ] **Step 1: Add repo field**

Add `SqlitePolicyInputRepo` to `ControlPlane`, initialize it in `new_unchecked`, include it in `Debug`, and add a test-support accessor following existing repo accessor style.

- [ ] **Step 2: Add use cases**

Implement:

```rust
pub async fn create_policy_input_set(
    &self,
    input: voom_policy::PolicyInputSetDraft,
) -> Result<voom_store::repo::policy_inputs::PolicyInputSet, VoomError>

pub async fn get_policy_input_set(
    &self,
    id: voom_store::repo::policy_inputs::PolicyInputSetId,
) -> Result<Option<voom_store::repo::policy_inputs::PolicyInputSet>, VoomError>

pub async fn list_policy_input_sets(
    &self,
) -> Result<Vec<voom_store::repo::policy_inputs::PolicyInputSetSummary>, VoomError>
```

No events are emitted in Sprint 3. The create use case should still open an explicit transaction so the control-plane path proves the intended transaction boundary.

- [ ] **Step 3: Add control-plane tests**

Add tests:

- `create_policy_input_set_round_trips_fixture`;
- `create_policy_input_set_rejects_invalid_model`;
- `list_policy_input_sets_is_deterministic`;
- `create_policy_input_set_failure_leaves_no_partial_rows`.

Run: `cargo test -p voom-control-plane --all-features policy_inputs`

Expected: policy input use-case tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-control-plane
git commit -m "feat: add policy input control-plane use cases"
```

## Task 6: Sprint 3 Acceptance Verification

**Files:**
- Modify only if needed: `docs/superpowers/specs/2026-05-22-voom-sprint-3-design.md`
- Optionally create: `docs/superpowers/specs/2026-05-22-voom-sprint-3-acceptance.md` if implementation discovers a real deferral

- [ ] **Step 1: Run targeted verification**

```bash
cargo test -p voom-policy --all-features
cargo test -p voom-store --all-features policy_inputs
cargo test -p voom-store --test migration_inventory --all-features
cargo test -p voom-control-plane --all-features policy_inputs
```

Expected: all commands exit 0.

- [ ] **Step 2: Run documentation scans**

```bash
rg -n "<incomplete-work-marker-regex>" docs/superpowers/specs/2026-05-22-voom-sprint-3-design.md docs/superpowers/plans/2026-05-22-voom-sprint-3-policy-inputs.md
git diff --check
```

Expected: marker scan has no output; diff check exits 0.

- [ ] **Step 3: Run full CI**

```bash
just ci
```

Expected: exits 0.

- [ ] **Step 4: Commit final acceptance docs if any changed**

If no acceptance doc changed, skip this commit. If an acceptance doc changed:

```bash
git add docs/superpowers/specs
git commit -m "docs: record sprint 3 acceptance"
```
