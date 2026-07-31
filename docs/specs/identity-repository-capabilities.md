# Identity repository capability interfaces

Status: draft
Date: 2026-07-31
Base: `refactor/2026-07-31` at `13f7d3954cd1`

## Context

`voom_store::repo::identity::IdentityRepo` currently exposes ingest and rename
orchestration plus persistence for media works, media variants, file assets,
file versions, file locations, identity evidence, and media snapshots. A caller
that needs one table family must depend on all 50 methods, and a second concrete
implementation would have to implement unrelated domains.

`SqliteIdentityRepo` remains the single shared concrete implementation. This
change narrows abstract dependencies without moving tables, changing SQL, or
changing transaction ownership.

No ADR is added. The change follows the existing one-way crate layering and
repository boundaries without choosing a new persistence architecture.

## Decision

Delete `IdentityRepo` and partition its 50 methods into public, object-safe
traits. The following ownership map is exhaustive and disjoint:

- `IngestRepo`: `record_discovered_file_in_tx`, `record_discovered_file`,
  `attach_local_hardlink_location_in_tx`, `reconcile_rename_in_tx`, and
  `reconcile_rename`.
- `MediaWorkRepo`: `create_media_work_in_tx`, `create_media_work`,
  `get_media_work`, `list_media_works`, and
  `update_media_work_provisional_in_tx`.
- `MediaVariantRepo`: `create_media_variant_in_tx`, `create_media_variant`,
  `get_media_variant`, `list_media_variants`, and
  `update_media_variant_provisional_in_tx`.
- `FileAssetRepo`: `create_file_asset_in_tx`, `create_file_asset`,
  `get_file_asset`, and `retire_file_asset_in_tx`.
- `FileVersionRepo`: `create_file_version_in_tx`, `create_file_version`,
  `get_file_version`, `get_file_version_in_tx`, `list_file_versions_by_asset`,
  `get_active_version_with_snapshot`, `require_active_file_versions_in_tx`,
  `list_live_file_versions`, and `retire_file_version_in_tx`.
- `FileLocationRepo`: `create_file_location_in_tx`, `get_file_location`,
  `get_file_location_in_tx`, `list_file_locations_by_version`,
  `list_live_file_locations_by_version`,
  `list_live_file_locations_by_version_in_tx`, `retire_file_location_in_tx`,
  `replace_file_location_in_tx`, and `update_file_location_value_in_tx`.
- `IdentityEvidenceRepo`: `record_identity_evidence_in_tx`,
  `get_identity_evidence`, `get_identity_evidence_in_tx`,
  `list_identity_evidence_by_target_in_tx`,
  `list_identity_evidence_by_target`,
  `list_live_identity_evidence_by_target`, `accept_identity_evidence_in_tx`,
  and `supersede_identity_evidence_in_tx`.
- `MediaSnapshotRepo`: `record_media_snapshot_in_tx`, `get_media_snapshot`,
  `get_media_snapshot_in_tx`, `get_media_snapshot_file_versions_in_tx`, and
  `list_media_snapshots_by_version`.

`IngestRepo` intentionally coordinates several identity tables atomically.
`FileVersionRepo` owns the active-version snapshot read because the file
version is the query anchor.

Each domain trait extends the existing `Repository` marker and is implemented
directly by `SqliteIdentityRepo`. Method bodies and public data types stay in
`identity.rs`; the refactor changes capability boundaries, not storage layout.

The aggregate commit safety gate context needs a trait object spanning file
versions, file locations, and evidence. Rust trait objects cannot combine
several non-auto traits directly, so add `CommitGateIdentityRepo` as a
capability marker with those three supertraits and a blanket implementation.
It declares no methods and is named for its consumer, avoiding a replacement
catch-all repository.

Every existing trait-object signature receives this exact replacement:

- `CommitGateContext.identity_repo`: `&dyn CommitGateIdentityRepo`.
- `scope::build_closure`: generic
  `R: FileVersionRepo + FileLocationRepo + ?Sized`.
- `scope::revalidate_evidence_in_tx`: generic
  `R: IdentityEvidenceRepo + ?Sized`.
- `prepare::run_phase_a_gate_in_tx`: `&dyn CommitGateIdentityRepo`.
- `authorize::run_phase_b_gate_in_tx`: `&dyn CommitGateIdentityRepo`.
- `finalize::finalize_applied_with_recovery_boundary`: generic
  `R: FileVersionRepo + FileLocationRepo + ?Sized`.
- `finalize::finalize_applied_inner`: generic
  `R: FileVersionRepo + FileLocationRepo + ?Sized`.
- `finalize::run_phase_c_trip_wires_in_tx`: generic
  `R: FileVersionRepo + FileLocationRepo + ?Sized`.
- `finalize::finalize_silent_path_in_tx`: generic
  `R: FileLocationRepo + ?Sized`.
- `finalize::dispatch_durable_mutation_in_tx`: generic
  `R: FileLocationRepo + ?Sized`.
- `lineage_commit::check_lineage_commit_leases_in_tx`:
  `&dyn FileLocationRepo`.
- `lineage_commit::build_lineage_closure`: `&dyn FileLocationRepo`.

The public context and Phase A/B orchestration use the composite because those
paths span all three domains. Phase C recomputes closure and mutates locations
but does not read evidence, so it is generic over versions and locations only.
Evidence revalidation names only evidence access. Mutation and lineage helpers
name only location access. Multi-domain private helpers use generic bounds so
they do not require another public composite marker.

There is no compatibility alias, deprecated umbrella trait, or inherent-method
forwarding layer. Callers import the smallest traits that provide the methods
they invoke. Existing specifications that name `IdentityRepo` are updated to
the new owning capability.

## Invariants and boundaries

- `SqliteIdentityRepo` remains the only production implementation and retains
  its `SqlitePool` ownership.
- All `_in_tx` methods continue using the caller's exact transaction.
- Ingest, rename, commit-gate closure computation, epoch checks, evidence
  validation, and event atomicity do not change.
- The commit safety gate remains generic over an object-safe repository
  capability and does not depend on the SQLite concrete type.
- `voom-control-plane` continues storing `SqliteIdentityRepo`; only its trait
  imports become capability-specific.
- No migration, serialized payload, event, error code, CLI output, dependency,
  or runtime behavior changes.

## Failure behavior

All methods preserve their current `VoomError` behavior. Splitting impl blocks
must not add transaction boundaries, map errors differently, or convert a
missing row into a different category. Composite capability dispatch is static
or ordinary trait-object dispatch with no new failure path.

## Compatibility and rollback

This is an intentional pre-release source-breaking replacement. All workspace
callers are updated atomically in one commit. Downstream users must import the
new capability traits; keeping `IdentityRepo` as a shim would leave the broad
contract in place and defeat the change.

Rollback is the inverse source refactor: restore the umbrella trait and its
single impl block. There is no durable-state rollback.

## Security and observability

The refactor does not alter authorization, secrets, SQL, logging, or emitted
events. Narrow traits reduce accidental access: generic consumers and trait
objects can call only the persistence domains they declare.

## Test strategy and acceptance criteria

- Existing identity repository unit tests continue to exercise every method on
  `SqliteIdentityRepo`.
- A compile-only test forms `dyn` references for all eight domain traits and
  `CommitGateIdentityRepo`, proving each remains object-safe under
  `async_trait` lowering.
- A generic compile-only theorem requires any `T` implementing
  `FileVersionRepo + FileLocationRepo + IdentityEvidenceRepo` to satisfy
  `CommitGateIdentityRepo`, proving the blanket implementation is not tied to
  SQLite.
- The evidence-only and Phase C helper declarations retain the exact generic
  bounds in the signature map; compilation fails if their bodies require an
  unrelated repository capability.
- The full finalize call chain compiles from the
  `&dyn CommitGateIdentityRepo` stored in `CommitGateContext`, proving the same
  unsized repository type flows through every generic Phase C helper without a
  trait-object coercion.
- Commit-safety-gate unit and integration tests compile through
  `dyn CommitGateIdentityRepo` and preserve all gate outcomes.
- Control-plane and CLI tests compile with explicit capability imports.
- `rg -n '\bIdentityRepo\b' crates --glob '*.rs'` finds no active Rust
  declaration, import, trait object, implementation, or Rustdoc reference.
- `rg -n '\bIdentityRepo\b' docs/specs --glob '*.md'` finds only this decision
  record's historical explanation. The two current specs that describe active
  APIs use the new owning capability. Historical records under
  `docs/superpowers/specs/` remain unchanged because they describe the API that
  existed when those plans were executed, not the current contract.
- `just fmt-check`, `just check-test-layout`, `just lint`, `just test`, and
  `just doc` pass with zero failures and zero warnings. The existing inventory
  of environment-gated ignored tests is unchanged; the new capability and
  object-safety tests are not ignored, and this change introduces no new skip.

## Dependencies and exclusions

Direct dependencies in scope are `identity.rs`, its public re-exports, every
workspace caller that must import a capability trait, commit-gate trait-object
signatures, active Rustdoc/comments under `crates/`, and the two current specs
that name the removed trait.

Excluded because runtime behavior is unchanged: migrations, repository SQL,
event payloads, error taxonomy, CLI contracts, and splitting `identity.rs` into
physical modules. An excluded concern is blocking if the trait split cannot
compile or preserve object safety without changing one of those boundaries.
