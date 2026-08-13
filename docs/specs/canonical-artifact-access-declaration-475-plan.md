# Implementation plan — canonical artifact access on byte-work tickets (#475)

Goal: byte-touching workflow tickets carry one strict, canonical declaration of the storage
they intend to access, validated identically on write and on read.

Spec: [`canonical-artifact-access-declaration-475.md`](canonical-artifact-access-declaration-475.md).
ADR: [`0068`](../adr/0068-byte-work-tickets-declare-canonical-artifact-access.md).
The spec is normative; this plan orders the work and names the seams. Where the two differ,
the spec wins and this file is wrong.

## Global constraints

- Branch `feat/canonical-artifact-access-475` off `main`. Never commit to `main`.
- Guardrails: `just ci` — `fmt-check`, `lint`, `check-test-layout`, `check-paused-time-db`,
  `check-control-plane-sql-boundary`, `check-check-constraint-bypass`,
  `check-payload-deny-unknown`, `check-adr-index`, `test`, `doc`, `deny`, `audit`. Each has a
  `-selftest` sibling. Run `just ci` before pushing; the pre-commit hooks run the fast subset.
- Zero warnings. `[workspace.lints]` has pedantic on and `panic`/`unwrap`/`expect` denied.
  In tests, propagate with `?` or use the repo's existing test-result aliases.
- Unit tests live in a sibling `<source>_test.rs` linked by `#[path]` from the parent
  (ADR 0004); `just check-test-layout` enforces it. Integration tests stay in `crates/*/tests/`.
- Never pair `tokio::time::pause()`/`advance()` with a real `SqlitePool` (ADR 0012). Drive
  DB tests on real time; control domain time through the injected `Clock`.
- No migration, no schema change, no new dependency. If a task appears to need one, stop.
- Durable payload structs carry `#[serde(deny_unknown_fields)]` on the real serde unit; a
  tagged enum is not annotated, and its variants are newtype variants over annotated content
  structs (ADR 0013). Inline tagged struct-variants are forbidden.
- Commit after each task, conventional-commit subject ≤72 chars, with the repo's
  `Co-Authored-By` trailer.

## File map

**Created**

| Path | Responsibility |
|---|---|
| `crates/voom-core/src/taxonomy/artifact_access_declaration.rs` (+ `_test.rs`) | the declaration vocabulary and its single validating constructor |
| `crates/voom-control-plane/src/workflow/plan/access_declaration.rs` (+ `_test.rs`) | `TicketStorageSource`, `declaration_for` — the operation-to-entries mapping |
| `crates/voom-control-plane/src/workflow/execution/executor/tickets_test.rs` | renderer assertions |
| `crates/voom-control-plane/src/cases/execution/remote_execution/acquire_test.rs` | candidate-loop containment |
| `crates/voom-control-plane/src/workflow/summary_test.rs` | pins the undecodable-skip behavior (#482 owns changing it) |

**Modified**

| Path | Change |
|---|---|
| `crates/voom-core/src/taxonomy/operation_kind.rs` | `is_byte_touching`, exhaustive |
| `crates/voom-core/src/taxonomy/ticket_operation.rs` | `WORKFLOW_OPERATION_NAMESPACE`, `NormalizedTicketOperation`, `normalize`, `matching_token` |
| `crates/voom-core/src/taxonomy/mod.rs`, `src/lib.rs` | module + re-exports |
| `crates/voom-store/src/repo/execution/workers.rs` | delete `normalized_worker_operation` and `WORKFLOW_OPERATION_PREFIX`; three call sites use `matching_token()`; conditional double bind |
| `crates/voom-store/src/repo/execution/leases.rs` | `acquire_guarded` uses `normalize`, rejects `UnknownNamespaced` |
| `crates/voom-control-plane/src/workflow/plan/ticket_payload.rs` | `declared_artifact_access`, shared `validate_artifact_access` on encode and decode, `normalize` replaces `ticket_operation` |
| `crates/voom-control-plane/src/workflow/plan/binding.rs` | `PolicyFileSource` gains root and non-optional location; `BranchContext.storage_source`; both renderers emit the two source fields |
| `crates/voom-control-plane/src/workflow/plan/expansion.rs` | `ScannerFile.file_location_id`; five `expand_*_completion` thread the source |
| `crates/voom-control-plane/src/workflow/plan/model.rs` | `#[cfg(test)] default_ci` byte-touching nodes gain `policy_target` |
| `crates/voom-control-plane/src/workflow/execution/executor/tickets.rs` | hoist resolution; privatize `resolve_policy_file_source`; delete the now-unreachable remux target-shape arm |
| `crates/voom-control-plane/src/workflow/execution/executor/expansion.rs` | skip-and-record an undecodable ticket instead of aborting the batch |
| `crates/voom-control-plane/src/operation_source.rs` | `select_location` → `pub(crate)` |
| `crates/voom-test-support` | helper seeding a storage root + file-location row |
| `docs/payload-contract-inventory.md`, `scripts/payload-contract-scope.txt` | register the new core module |

## Task order

Each task ends at something testable on its own, and each is a commit.

### Task 1 — `voom-core`: the declaration vocabulary

Creates `artifact_access_declaration.rs` and its sibling test. No other crate changes, so
this compiles and tests in isolation.

Interfaces produced (consumed by Tasks 4 and 5): `ArtifactAccessRight`,
`StorageRootAccess`, `FileLocationAccess`, `ExistingArtifactAccess`, `PlannedArtifactAccess`,
`ArtifactAccessTarget`, `ArtifactAccessEntry`,
`ArtifactAccessDeclaration::new(Vec<ArtifactAccessEntry>) -> Result<Self, VoomError>`,
`entries()`.

Steps, in TDD order:
1. Write `artifact_access_declaration_test.rs` with the seven rejection cases from spec §2,
   asserting the exact message strings. Run `cargo test -p voom-core artifact_access` and
   confirm it fails to compile — that is the red state for a new module.
2. Write the types and `new`. Re-run; confirm green.
3. Add the frozen canonical-encoding fixture test (one entry of each of the four target
   variants, multi-right entries, byte-exact JSON both directions). Confirm red, then satisfy
   it by fixing the derive order rather than the fixture.
4. Add the exhaustive 24-permutation ordering test from spec §"criterion 3".
5. Register the module in `taxonomy/mod.rs` and `lib.rs`; add it to
   `scripts/payload-contract-scope.txt` and `docs/payload-contract-inventory.md`.
6. `just check-payload-deny-unknown && just check-test-layout && just lint && cargo test -p voom-core`.

### Task 2 — `voom-core`: `is_byte_touching`

Exhaustive match over all fifteen `OperationKind` variants, no wildcard. Sibling test asserts
the twelve/three split and that `ALL` is covered.

### Task 3 — `voom-core`: one ticket-kind normalization

`WORKFLOW_OPERATION_NAMESPACE`, `NormalizedTicketOperation` with
`Known { kind, namespaced }` / `CustomLocal` / `UnknownNamespaced`, infallible `normalize`,
`operation_kind`, `matching_token`. Tests per spec §4's criterion-4 list.

Consumed by Tasks 4 and 6. Nothing outside `voom-core` changes yet, so the old
`normalized_worker_operation` still exists and the workspace stays green.

### Task 4 — `voom-store`: adopt the normalization, delete the duplicate

Deletes `normalized_worker_operation` and the local `WORKFLOW_OPERATION_PREFIX`. The three
`workers.rs` call sites take `matching_token()` and never raise; the double bind at
`workers.rs:1158` becomes conditional on `Known`. `leases.rs:319` uses `normalize` and rejects
`UnknownNamespaced` with a database error naming the field.

Red first: add the `workers_test.rs` case that today aborts on
`synthetic.workflow.operation.` (empty suffix) and the `leases_test.rs` fail-closed case.
Existing tests using `synthetic.workflow.operation.test` / `.extract` are updated here.

This task is independently shippable — it fixes the two-normalizations defect on its own.

### Task 5 — control plane: the mapping and the payload gate

`access_declaration.rs` with `TicketStorageSource` and `declaration_for` (table-driven over
all fifteen operations × both source variants, per spec §6), then `ticket_payload.rs` gains
`declared_artifact_access` and the shared `validate_artifact_access` with the four rules and
their exact messages.

Renderers are not touched yet, so the workspace will not build until Task 6. Land 5 and 6
together if the intermediate state cannot compile.

### Task 6 — control plane: producers

`PolicyFileSource` gains `storage_root_id` and a non-optional `location_id`;
`resolve_policy_file_source` routes both target shapes through `select_location`;
`insert_policy_file_source` and `render_default_payload_with_fan_out` emit
`source_storage_root_id` and `source_location_id`; resolution hoists into
`render_node_ticket`; the unreachable remux target-shape arm is deleted;
`BranchContext.storage_source` is added.

### Task 7 — control plane: expansion threading

`ScannerFile.file_location_id`; the five `expand_*_completion` functions build the child's
`TicketStorageSource` from the parent's `rendered_payload`;
`executor/expansion.rs` skips and records an undecodable ticket instead of aborting the batch.

### Task 8 — fixture migration

`default_ci` byte-touching nodes gain `policy_target`; the `voom-test-support` helper seeds a
storage root and file-location row; every test seeding a scanner result gains
`file_location_id`. Verified reach at audit time: 8 files across `voom-fakes`,
`voom-fake-support`, `voom-cli`, `voom-control-plane` seed scanner results; 28 files reference
`synthetic.workflow.operation.`.

Do not weaken or delete the `durable_workflow_test.rs` end-to-end coverage to make this pass.

### Task 9 — full guardrails

`just ci` bare, no pipes. Then the branch review loop.

## Resume facts

Branch `feat/canonical-artifact-access-475`; `BASE_BRANCH` `main`; guardrails `just ci`;
design committed; audit report at `.agent/oathbind/475-canonical-artifact-access.md`
(git-ignored). Open follow-ups filed during design: #480 drain tooling, #481 frozen-location
recovery, #482 summary under-reporting. Filed during build: #483 stale
`.voom-ffprobe-sibling.lock` (pre-existing test-support defect, not caused by this change).

### Build state

All nine tasks implemented. Commits, oldest first:

| Task | Commit | Subject |
|---|---|---|
| 1 | `186070c3` | `feat(core): add canonical artifact access declaration` |
| 2 | `03198e75` | `feat(core): classify byte-touching operation kinds` |
| 3 | `ca759480` | `feat(core): add one ticket-kind normalization` |
| 4 | `4239373f` | `fix(store): adopt one normalization and fail closed on unknown kinds` |
| 5–8 | `d30153df` | `feat(control-plane): require canonical artifact access on byte-work tickets` |

Tasks 5–8 landed together: the dead-code deny makes the intermediate state unbuildable,
as the plan anticipated.

### Divergences from the plan, and why

- `artifact_access_declaration.rs`, not `artifact_access.rs` — the longer name says what
  the module holds.
- `crates/voom-store/src/test_support.rs` gained `seed_test_rooted_location` and
  `TEST_FILE_LOCATION_ID` / `TEST_FILE_VERSION_ID`, rather than `voom-test-support`. The
  seeding is raw SQL against tables `voom-store` owns, and `voom-store::test_support`
  already holds `seed_test_storage_root`, which the new helper calls.
- `crates/voom-fake-support/src/results.rs` and `validation.rs` were not on the plan's
  file map. Both were forced by the change: the fake scanner must report
  `file_location_id` per result entry, and `protocol_payload_without_runtime_metadata`
  must strip the two new source fields, because the default `transcode_video` rendered
  payload *is* a serialized `TranscodeVideoRequest` and that type denies unknown fields.
- Four executor assertions on absolute SQLite rowids (`result_file_version_id == 2`) now
  assert "a new version distinct from the source". Seeding a fixture row shifts the rowid
  sequence; the absolute value was never the property under test.
- Three tests whose premise this change removes were inverted rather than deleted:
  `non_policy_remux_root_ticket_uses_default_payload` →
  `byte_touching_root_node_without_a_target_fails_instead_of_rendering`;
  `policy_remux_payload_omits_absent_source_location` →
  `..._always_carries_its_source_root_and_location`; and both retired-location tests now
  expect `NotFound`, which is how the shared `require_live_rooted_location` classifies it.

### Next

Quest steps 6–9: branch adversarial review against `main`, `$detect-evil` security pass
(the diff qualifies — it changes what a ticket may claim about storage it opens),
`$dispel`, then PR with a `WORK:REVIEW` comment.
