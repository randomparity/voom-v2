# Typed scheduler artifact access

- Status: Draft
- Date: 2026-07-31
- Related: [ADR 0053](../../adr/0053-artifact-access-mode-is-core-domain-vocabulary.md)
- Desloppify finding:
  `review::.::holistic::mid_level_elegance::scheduler_artifact_access_string_seam`

## Problem

`ArtifactAccessMode` is currently defined in `voom-store`, even though the
vocabulary governs scheduler selection and remote dispatch as well as durable
rows. `voom-scheduler` therefore represents `WorkerCandidate.artifact_access`
and `SelectedCandidate.access_mode` as strings. The control plane must convert a
scheduler-selected string back into the store enum with
`artifact_access_mode_from_scheduler`.

That seam permits states the scheduler should make unrepresentable: a selected
candidate can theoretically contain a token the artifact-plan repository cannot
store. It also duplicates the three tokens across store, scheduler, and control
plane code.

## Goal

Make the supported artifact-access vocabulary a core domain type and carry it
through scheduler candidates and selections without changing any persisted or
serialized token or any scheduling behavior.

## Non-goals

- Changing the `worker_capabilities.artifact_access` JSON schema or registration
  input from `Vec<String>`.
- Rejecting unknown advertised modes at registration or database read time.
- Adding, removing, or reprioritizing artifact-access modes.
- Changing the `artifact_access_plans.selected_access_mode` schema.
- Changing scheduler scores, explanation JSON, dispatch JSON, or error codes.

## Design

### Core vocabulary

Move `ArtifactAccessMode` to a sibling-tested core taxonomy module and re-export
it from `voom_core`. Preserve its variants and `snake_case` serde representation:

| Variant | Wire/database token |
|---|---|
| `SharedMount` | `shared_mount` |
| `ControlPlanePlaceholder` | `control_plane_placeholder` |
| `StagedOutputPlaceholder` | `staged_output_placeholder` |

The type provides:

- `as_str() -> &'static str` for explicit persistence and comparison boundaries;
- `from_wire(&str) -> Option<Self>` for forward-compatible recognition;
- `parse_database(field, value) -> Result<Self, VoomError>` for closed durable
  columns that must fail loudly with field context.

`voom-store` removes its local definition and imports/re-exports the core type
through its existing repository surface so current store callers do not need a
second source of truth. Durable plan decoding uses `parse_database`.

### Typed scheduler seam

Change these scheduler fields:

- `WorkerCandidate.artifact_access: Vec<ArtifactAccessMode>`
- `SelectedCandidate.access_mode: ArtifactAccessMode`

`select_access_mode` returns the enum by value. Its priority remains shared mount,
then control-plane placeholder, then staged-output placeholder. Scoring and factor
rendering use exhaustive enum matches. `serde_json::json!` serializes the enum to
the same snake-case explanation token.

The control-plane candidate projection maps persisted advertisement strings with
`ArtifactAccessMode::from_wire` and discards unknown tokens. This preserves both
current cases:

- only unknown modes: no supported mode, so the candidate is rejected with
  `unsupported_artifact_access`;
- unknown plus known modes: the known mode remains selectable.

The scheduler selection is already the exact type required by
`NewArtifactAccessPlan`, so `artifact_access_mode_from_scheduler` is deleted.

### Contract boundaries

No migration is required. The SQLite `CHECK`, serialized enum token, scheduler
explanation, and remote artifact access plan all retain the existing strings.
Worker advertisements deliberately remain raw at their wire and persistence
boundary so a newer worker can advertise an additional mode without making an
older control plane reject the entire capability record.

## Failure behavior

- An unknown value in the closed `artifact_access_plans.selected_access_mode`
  column remains a contextual database error.
- Unknown worker-advertisement values are ignored for scheduler support, as today.
- An empty recognized-mode projection remains an ordinary ineligible candidate,
  not an internal error.
- The selected-mode conversion error path disappears because the scheduler cannot
  construct an unsupported `SelectedCandidate.access_mode`.

## Acceptance criteria

1. Core tests prove all three serde/as-string tokens and reject an unknown wire
   token; database parsing reports the supplied field and value.
2. Scheduler public candidate and selection fields use `ArtifactAccessMode`, and
   scheduler tests prove the existing priority, scores, and explanation strings.
3. A control-plane test proves an advertisement containing an unknown token plus a
   known token still leases using the known mode.
4. The existing unknown-only remote acquisition test still produces
   `unsupported_artifact_access` without leasing.
5. Store artifact access plan tests pass without changing stored values or JSON.
6. `artifact_access_mode_from_scheduler` and its string match are absent.
7. `just ci` passes with zero warnings.

## Verification

- `cargo test -p voom-core artifact_access_mode`
- `cargo test -p voom-scheduler`
- `cargo test -p voom-control-plane remote_acquire`
- `cargo test -p voom-store artifact_access_plan`
- `just check-adr-index`
- `just ci`
