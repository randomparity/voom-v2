---
name: voom-sprint-3-design
description: Sprint 3 design for the policy-domain input model, deterministic fixtures, and durable SQLite policy input-set records.
status: proposed
date: 2026-05-22
sprint: 3
branch: feat/sprint-3
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-22-voom-mvp-roadmap-rescope-design.md
---

# VOOM Sprint 3 — Policy Domain Model And Snapshot Inputs

## 1. Purpose

Sprint 3 defines the policy-domain input layer that later parser,
compiler, planner, CLI, daemon, and UI work will consume. It does not
define policy text syntax and it does not create execution plans. It
creates typed Rust models, deterministic fixtures, and durable SQLite
records for the facts a future policy evaluation needs.

The central concept is a `PolicyInputSet`: a versioned, named bundle of
policy inputs that can be authored by tests and fixtures now, and later
linked to parsed policy versions or user-facing policy bindings.

## 2. Scope

Sprint 3 delivers:

- Media snapshot input model.
- Identity-evidence input model.
- Bundle target input model.
- Quality profile selection model.
- Issue input model.
- Deterministic compliant and noncompliant synthetic media fixtures.
- SQLite persistence for policy input sets and their child inputs.
- Repository and control-plane use cases for transactional input-set
  creation, lookup, and listing.
- Acceptance matrix mapping original policy input requirements to a
  model, table, fixture, or explicit later-sprint deferral.

Sprint 3 explicitly does not deliver:

- Policy text grammar.
- Parser, validator, or compiled policy model.
- Plan DAG generation.
- CLI plan, dry-run, or policy execution commands.
- Synthetic execution through workers.
- Plugin-defined policy operations.
- UI editing or reporting.

## 3. Architecture

`voom-policy` owns the public domain model. It is a pure Rust crate with
serde-enabled types, validation helpers, fixture loaders, and no
database dependency. Its responsibility is to make the Sprint 3 input
contract explicit and deterministic.

`voom-store` owns persistence. It adds a `policy_inputs` repository and
the `0006_policy_inputs.sql` migration. The schema uses typed columns
and `CHECK` constraints for stable vocabularies, and JSON only for
provenance, dimension maps, stream details, and future-compatible
payloads where the architectural spec expects extensibility.

`voom-control-plane` owns write use cases. It records a whole input set
transactionally so fixtures cannot persist partial policy state. The
control plane should expose create/get/list operations over the
repository, but no CLI command is required in Sprint 3.

Events are not the primary acceptance surface for this sprint. If an
existing policy-relevant event kind already fits a write, the control
plane may compose it in the same transaction. Sprint 3 should not expand
the event vocabulary solely to narrate fixture insertion; the durable
input-set rows and fixture tests are the source of truth.

### 3.1 Policy identity compatibility

`PolicyInputSet` is not a policy document, policy version, or accepted
policy. Its ids must not be written into
`identity_evidence.accepted_policy_id`.

Sprint 1 left `identity_evidence.accepted_policy_id` nullable for the
future policy registry. Sprint 3 deliberately does not fill that hook:
the policy id space belongs to Sprint 4, when parser and compiler work
introduces durable policy/version identity. Sprint 3 input sets may
later link to those policy versions, but input-set ids and policy ids
remain separate namespaces.

## 4. Domain Model

### 4.1 PolicyInputSet

`PolicyInputSet` is the aggregate root. It contains:

- stable numeric id after persistence;
- unique `slug`;
- display name;
- `schema_version`;
- source kind: `fixture`, `test`, `imported`, or `manual`;
- creation timestamp;
- optional description;
- zero or more fixture labels.

The implementation should make fixture labels unique. The canonical
Sprint 3 labels are:

- `synthetic_compliant_baseline`;
- `synthetic_noncompliant_transcode_needed`.

Additional labels may be added only when they map to an acceptance case.

### 4.2 MediaSnapshotInput

`MediaSnapshotInput` normalizes probe-like facts into policy input
shape. It is not raw ffprobe output and it is not worker output.

The model includes:

- target scope;
- container;
- stream summary JSON;
- video codec, width, height, HDR state, bitrate, and duration when
  known;
- audio language/layout facts;
- subtitle language and disposition facts;
- health flags such as missing, corrupt, unsupported, or incomplete;
- optional link to an existing `media_snapshots` row.

### 4.3 IdentityEvidenceInput

`IdentityEvidenceInput` describes identity assertions policy may use.
It includes:

- target scope;
- assertion type;
- provider and provider version;
- confidence in the closed interval `0.0..=1.0`;
- provenance JSON;
- observed timestamp;
- optional link to an existing durable `identity_evidence` row.

Sprint 3 permits inline evidence inputs so synthetic fixtures do not
need to pre-seed the full identity registry.

### 4.4 BundleTargetInput

`BundleTargetInput` describes desired bundle-level facts, not execution
steps. It includes:

- target scope;
- bundle member role;
- desired state such as `required`, `allowed`, `forbidden`, or
  `preferred`;
- optional language, label, and disposition constraints;
- optional artifact expectation JSON.

This model is enough to express primary video, external subtitles,
commentary audio, posters, NFO files, trailers, transcripts, thumbnails,
and report attachments without creating plan nodes.

### 4.5 QualityProfileSelection

`QualityProfileSelection` records what "better" means for the input
set. It includes:

- target scope;
- profile name;
- profile version;
- optional dimension weights JSON.

It should support named profiles from the architectural spec, including
`balanced-home`, `small-direct-play`, `preserve-source`,
`anime-subtitle-focused`, and `4k-plus-hd-fallback`, without hard-coding
that list as the only accepted profile names.

### 4.6 IssueInput

`IssueInput` records durable issue facts relevant to policy decisions.
It includes:

- target scope;
- issue kind;
- severity;
- priority;
- input state such as `open`, `accepted`, `suppressed`, or `planned`;
- reason;
- provenance JSON;
- optional link to an existing durable `issues` row.

Sprint 3 does not create, update, resolve, or suppress real issues from
policy evaluation. That behavior belongs to Sprint 6.

### 4.7 Target Scope

Every scoped child input names exactly one target. The allowed concrete
target kinds are:

- media work id;
- media variant id;
- bundle id;
- file asset id;
- file version id;
- file location id.

The allowed synthetic target form is a synthetic key declared by the
same input set. Synthetic targets are required for deterministic
fixtures that should not depend on a pre-seeded SQLite identity graph.
Real durable ids are used when available.

### 4.8 PolicySyntheticTarget

`PolicySyntheticTarget` declares a fixture-owned logical object before
child inputs can reference it. It contains:

- owning input-set id;
- unique `synthetic_key` within that input set;
- declared target kind: `media_work`, `media_variant`, `asset_bundle`,
  `file_asset`, `file_version`, or `file_location`;
- optional display name.

The same synthetic key within one input set denotes the same logical
object across all child tables. A child row that references a synthetic
key must also declare the same target kind as the corresponding
`PolicySyntheticTarget`. Reusing the same key for different target kinds
inside one input set is invalid.

## 5. Persistence

Migration `0006_policy_inputs.sql` should add:

- `policy_input_sets`;
- `policy_input_set_fixture_labels`;
- `policy_input_synthetic_targets`;
- `policy_media_snapshot_inputs`;
- `policy_identity_evidence_inputs`;
- `policy_bundle_target_inputs`;
- `policy_quality_profile_selections`;
- `policy_issue_inputs`.

The schema should follow existing repository conventions:

- `STRICT` tables.
- `created_at` stored as ISO-8601 text.
- `epoch` on mutable root rows where optimistic concurrency is useful.
- `CHECK` constraints for source kind, target kind, desired state, and
  issue input state.
- `json_valid` checks for JSON payload columns.
- child tables cascade from `policy_input_sets` so failed or deleted test
  input sets do not leave orphaned policy input rows.
- indexes for listing by input-set id and resolving fixture labels.

The root slug is unique. Fixture labels are unique globally and unique
per input set. Child rows should preserve deterministic ordering with an
integer `ordinal` column or insertion-order query rule, so JSON and DB
round trips produce stable fixture output.

Synthetic targets must use a `(policy_input_set_id, synthetic_key)`
unique constraint. Child tables that support synthetic targets must
store enough shape to enforce same-input-set lookup of the declared
target kind. The implementation may enforce that cross-table reference
through a foreign-key-friendly synthetic-target id or through repository
validation plus SQL uniqueness, but it must be impossible for persisted
fixture data to use an undeclared synthetic key.

## 6. Validation

`voom-policy` validation should enforce:

- an input set has at least one media snapshot input or bundle target
  input;
- slug and fixture labels are non-empty stable tokens;
- each scoped child input names exactly one target;
- every synthetic-key target references a declared
  `PolicySyntheticTarget` in the same input set;
- a synthetic key is never reused for multiple target kinds within one
  input set;
- evidence confidence is within `0.0..=1.0`;
- profile names and provider names are non-empty;
- JSON-like fields serialize deterministically in fixtures;
- fixture labels match known acceptance cases unless explicitly added to
  the acceptance matrix.

`voom-store` should duplicate shape-critical constraints in SQL so
invalid rows cannot be inserted by bypassing the Rust model.

## 7. Fixtures

Sprint 3 must ship deterministic fixture files under the policy crate.
The exact path can follow the implementation's test-resource convention,
but the fixtures must be crate-owned and loaded by tests rather than
constructed only in Rust code.

Required fixtures:

- `synthetic_compliant_baseline`: expresses a media item that already
  satisfies expected container, codec, audio language, subtitle, bundle,
  quality-profile, and identity-evidence facts.
- `synthetic_noncompliant_transcode_needed`: expresses a media item that
  does not satisfy the desired policy inputs and would later require
  transformation or issue creation, without generating a plan in Sprint
  3.

Each fixture must round-trip:

- JSON to Rust model to JSON;
- Rust model to SQLite to Rust model;
- persisted input set to deterministic JSON projection.

## 8. Testing

Sprint 3 verification includes:

- `voom-policy` unit tests for domain validation.
- Fixture round-trip tests.
- `voom-store` migration inventory tests including migration 0006.
- `PolicyInputRepo` repository round-trip tests.
- Transaction rollback test proving an invalid child row leaves no
  partial input set.
- `voom-control-plane` use-case tests for create/get/list.
- Documentation scan for incomplete-work markers.
- `just ci`.

Tests must use the existing sibling unit-test layout and existing store
test-support patterns.

## 9. Acceptance And Traceability Matrix

| Policy input area | Sprint 3 coverage | Fixture coverage | Deferral |
|---|---|---|---|
| Media policy inputs: container, codec, audio language, subtitles, and health facts. | `MediaSnapshotInput`, `BundleTargetInput`, `policy_media_snapshot_inputs`, `policy_bundle_target_inputs`. | Both required fixtures include snapshot and bundle-target rows. | Policy text grammar, compliance reports, and plan generation are Sprint 4 through Sprint 6. |
| Identity evidence inputs. | `IdentityEvidenceInput`, optional links to existing `identity_evidence`, inline fixture evidence. | Both required fixtures include identity evidence using declared synthetic targets. | Accepting evidence under a policy id is Sprint 4+ policy registry work. |
| Bundle and sidecar target inputs. | `BundleTargetInput` plus bundle-role vocabulary. | The compliant fixture includes satisfied bundle roles; the noncompliant fixture includes at least one missing or forbidden target fact. | Bundle commit, rename, rollback, and artifact production are later commit/planner work. |
| Quality/scoring policy inputs. | `QualityProfileSelection` with profile name, version, and optional dimension weights. | Both required fixtures name the active scoring profile. | Scoring math and quality compliance reports are Sprint 6+ work. |
| Issue policy inputs. | `IssueInput` can represent relevant open, accepted, suppressed, or planned issue facts. | The noncompliant fixture includes at least one issue input that explains the future action. | Creating or updating durable issues from policy evaluation is Sprint 6. |
| Retention policy inputs. | Covered only as quality profile selections and identity evidence facts. | Noncompliant fixture may express duplicate or lower-quality facts as evidence/issue inputs. | Archive, delete, keep-best, and approval behavior are later retention/planner work. |
| Safety policy inputs. | Covered only where evidence and issue facts are needed as later safety-gate inputs. | Fixtures may include evidence needed by future safety checks, but no gate executes. | Approval gates, backup requirements, rollback, and commit safety decisions are later planner/execution work. |
| External-system policy inputs. | Covered only as identity evidence, issue facts, and provenance payloads. | Fixtures may name external providers in evidence provenance. | External sync jobs, path mapping behavior, writes, and refresh operations are later external-system work. |
| Runtime use policy inputs. | No new Sprint 3 input model; use leases are already durable control-plane state. | No required fixture coverage. | Using active leases in policy/planner decisions is deferred to planner/scheduler work. |
| Scheduling policy inputs. | No Sprint 3 model. | No required fixture coverage. | Scheduling priorities, windows, throttles, locality, and worker eligibility are scheduler/planner sprints. |
| Durable policy input records. | Migration 0006, `PolicyInputRepo`, control-plane create/get/list use cases. | Required fixtures round-trip through SQLite. | Durable policy document/version identity is Sprint 4. |
| Synthetic fixture targets. | `PolicySyntheticTarget`, `policy_input_synthetic_targets`, same-input-set synthetic-key validation. | Both required fixtures declare every synthetic target before child rows reference it. | Real identity seeding remains optional for Sprint 3 fixtures. |

## 10. Implementation Order

1. Add `voom-policy` dependencies and domain model modules.
2. Add fixture files and model/fixture tests.
3. Add migration 0006.
4. Add `voom-store::repo::policy_inputs`.
5. Add repository tests, including rollback behavior.
6. Add `ControlPlane` policy-input use cases and tests.
7. Add traceability-matrix closeout notes if implementation discovers a
   legitimate deferral.

## 11. Open Decisions

No open product decisions remain for Sprint 3. Exact Rust module names,
helper names, and fixture file paths are implementation details as long
as they preserve the crate boundaries and acceptance matrix above.
