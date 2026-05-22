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

Every scoped child input names exactly one target. The allowed target
kinds are:

- media work id;
- media variant id;
- bundle id;
- file asset id;
- file version id;
- file location id;
- synthetic key.

The synthetic key form is required for deterministic fixtures that
should not depend on a pre-seeded SQLite identity graph. Real durable ids
are used when available.

## 5. Persistence

Migration `0006_policy_inputs.sql` should add:

- `policy_input_sets`;
- `policy_input_set_fixture_labels`;
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

## 6. Validation

`voom-policy` validation should enforce:

- an input set has at least one media snapshot input or bundle target
  input;
- slug and fixture labels are non-empty stable tokens;
- each scoped child input names exactly one target;
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
- Documentation placeholder scan.
- `just ci`.

Tests must use the existing sibling unit-test layout and existing store
test-support patterns.

## 9. Acceptance Matrix

| Requirement | Sprint 3 artifact | Notes |
|---|---|---|
| Media snapshots can feed future policy evaluation. | `MediaSnapshotInput`, `policy_media_snapshot_inputs`, compliant/noncompliant fixtures. | Raw probe parsing is later worker/provider work. |
| Identity evidence can influence future policy decisions. | `IdentityEvidenceInput`, `policy_identity_evidence_inputs`. | Existing `identity_evidence` ids may be linked, but inline fixture evidence is allowed. |
| Bundle targets can express primary media and sidecar expectations. | `BundleTargetInput`, `policy_bundle_target_inputs`. | No bundle commit or rename behavior in Sprint 3. |
| Quality profile selection is explicit. | `QualityProfileSelection`, `policy_quality_profile_selections`. | Scoring math and compliance reports are later work. |
| Existing or expected issues can be represented. | `IssueInput`, `policy_issue_inputs`. | Durable issue creation from noncompliance is Sprint 6. |
| Fixtures can express compliant and noncompliant synthetic media. | Two required fixture labels and files. | No parser, planner, or execution needed. |
| Policy input records are durable. | Migration 0006, repository tests, control-plane use cases. | Includes SQLite because Sprint 3 must feed later durable planner work. |

## 10. Implementation Order

1. Add `voom-policy` dependencies and domain model modules.
2. Add fixture files and model/fixture tests.
3. Add migration 0006.
4. Add `voom-store::repo::policy_inputs`.
5. Add repository tests, including rollback behavior.
6. Add `ControlPlane` policy-input use cases and tests.
7. Add acceptance-matrix closeout notes if implementation discovers a
   legitimate deferral.

## 11. Open Decisions

No unresolved product decisions remain for Sprint 3. Exact Rust module
names, helper names, and fixture file paths are implementation details as
long as they preserve the crate boundaries and acceptance matrix above.
