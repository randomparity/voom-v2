---
name: voom-sprint-4-design
description: Sprint 4 design for the v1-compatible VOOM DSL parser, validator, compiled policy model, diagnostics, and durable policy registry.
status: proposed
date: 2026-05-22
sprint: 4
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-22-voom-mvp-roadmap-rescope-design.md
  - docs/superpowers/specs/2026-05-22-voom-sprint-3-design.md
  - /home/dave/src/voom-v1/docs/dsl-reference.md
---

# VOOM Sprint 4 - Policy Parser, Validator, And Compiled Model

## 1. Purpose

Sprint 4 introduces the `.voom` policy language for VOOM v2. The
language uses the VOOM v1 block-based DSL as its syntax backbone, while
compiling into v2-native control-plane policy intent instead of v1
runtime execution concepts.

A `.voom` policy file is parsed, validated, compiled, and persisted as
an immutable policy version linked to a stable policy document. Later
CLI, API, planner, and orchestrator work will read the compiled policy
version from SQLite. Workers must not interpret `.voom` source directly;
they will eventually receive typed operation payloads derived by the
control plane from compiled policies and durable plans.

## 2. Scope

Sprint 4 delivers:

- `.voom` source parser based on the v1 block DSL.
- AST with source spans for machine-readable diagnostics.
- Semantic validator for the Sprint 4 accepted subset.
- V2-native compiled policy model.
- Stable diagnostic model with parse, validation, and compile stages.
- Golden valid policy fixtures adapted from v1 examples.
- Golden invalid diagnostic fixtures.
- Durable policy registry tables for policy documents and immutable
  policy versions.
- Store repository and control-plane use cases for creating, versioning,
  retrieving, listing, and compiling policies.
- Closeout acceptance matrix for grammar scope, diagnostics, and
  compatibility rules.

Sprint 4 explicitly does not deliver:

- Compliance reports.
- Plan DAG generation.
- Scheduling priority execution.
- Worker payload generation.
- Worker dispatch or media mutation.
- CLI run, dry-run, or plan commands.
- UI editing.
- Plugin-defined policy operation schemas.
- Remote policy loading.

## 3. Language Scope

Sprint 4 adopts `.voom` files using v1-style syntax:

```voom
policy "production-normalize" {
  metadata {
    version: "1.0.0"
    description: "Normalize a library item"
  }

  config {
    languages audio: [eng, und]
    languages subtitle: [eng, und]
    on_error: continue
  }

  phase containerize {
    container mkv
  }

  phase normalize {
    depends_on: [containerize]
    keep audio where lang in [eng, und]
    remove attachments where not font
  }
}
```

The accepted Sprint 4 syntax includes:

- `policy "<name>" { ... }`;
- `//` line comments, v1 identifier tokens, strings, numbers, booleans,
  lists, colon-delimited settings, and free-form whitespace;
- optional `metadata` block;
- optional `config` block;
- one or more `phase` blocks;
- phase controls: `depends_on`, `skip when`, `run_if`, and `on_error`;
- operations: `container`, `keep`, `remove`, `order tracks`,
  `defaults`, track `actions`, `clear_tags`, `set_tag`, and
  `delete_tag`;
- conditional policy logic: `when` and `rules`;
- conditions: `exists`, `count`, field comparison, field existence,
  built-in predicates, `and`, `or`, `not`, and parentheses;
- filters: language, codec, channels, commentary, forced, default,
  font, title contains, title matches, and boolean composition;
- primitive values: strings, numbers, booleans, identifiers, and lists.

Core field-path roots in Sprint 4 are limited to media facts already
represented by Sprint 3 policy inputs, including `video`, `audio`,
`subtitle`, `subtitles`, `attachment`, `attachments`, `container`,
`identity`, `quality`, `issue`, and `bundle`. Extension paths must start
with a reserved extension root such as `plugin` or `external`. Unknown
core-looking roots are errors; unknown namespaces below a reserved
extension root warn so later plugin and external-system registries can
tighten them without changing the parser.

Sprint 4 should parse and compile `when` and `rules` because they are
policy intent, not execution infrastructure. It should reject
`transcode`, `synthesize`, and `verify` with stable deferred-feature
diagnostics. Those operations imply planner or worker semantics and
belong in later sprints.

The accepted subset is a compatibility contract, not an invitation to
accept every v1 token. Parser tests must prove the examples above plus
the named valid fixtures parse, and invalid fixture tests must prove
unsupported v1-only forms fail with stable diagnostics. Unknown
top-level blocks, unknown phase statements, and unknown operation names
must be validation errors unless this design explicitly lists them as
warnings. `extend` inside a phase is invalid in Sprint 4 because parent
composition is deferred with `extends`.

`extends` is reserved. Bundled and `file://` composition are useful v1
features, but Sprint 4 should not load parent policies. A policy using
`extends` should produce a stable validation diagnostic explaining that
composition is deferred.

## 4. Architecture

`voom-policy` owns the language stack. It remains a pure Rust crate with
no database dependency. Sprint 4 adds modules for:

- AST and source spans;
- parser and grammar;
- semantic validation;
- compiler;
- compiled policy model;
- diagnostics;
- fixture loading and golden projections.

The parser and compiler may reuse v1 concepts, names, and examples, but
must not depend on v1 crates or v1 runtime domain types. V2 compiled
policy types live in `voom-policy` and describe durable policy intent
for later planner work.

`voom-store` owns persistence. Migration `0007_policy_registry.sql`
adds durable policy document and policy version tables. The repository
stores accepted compiled policy versions and returns immutable version
records.

`voom-control-plane` owns use cases. It compiles policy source through
`voom-policy`, stores accepted versions through `voom-store`, and
returns stable diagnostics for rejected source. It should expose:

- create a policy document with its first accepted version;
- add a new accepted version to an existing document;
- compile policy source without persistence;
- get a policy document;
- list policy documents;
- get and list policy versions.

No event-log behavior is required for Sprint 4 acceptance. If a future
event vocabulary needs policy-created or policy-version-added facts,
that should be designed when CLI/API surfaces depend on those events.

## 5. Durable Policy Registry

Sprint 4 introduces a policy id namespace that is distinct from Sprint 3
policy input-set ids. `PolicyInputSetId` must never be treated as a
policy document id or policy version id.

The schema should add:

- `policy_documents`;
- `policy_versions`.

`policy_documents` contains stable policy identity:

- id;
- globally unique slug using the same stable-token rules as Sprint 3
  input-set slugs;
- display name copied from the policy name unless explicitly overridden
  by a control-plane creation draft;
- created timestamp;
- optional current accepted version id;
- epoch for optimistic concurrency on current-version and display-name
  updates.

`policy_versions` contains immutable accepted source:

- id;
- policy document id;
- monotonically increasing version number within the document;
- `.voom` source text;
- source hash;
- language/schema version;
- compiled JSON projection;
- created timestamp.

Migration `0007_policy_registry.sql` must enforce the identity model in
SQLite, not only in Rust:

- `policy_documents.slug` is unique;
- `policy_versions(policy_document_id, version_number)` is unique;
- `policy_versions(policy_document_id, source_hash)` is unique so a
  repeated source for the same document returns or reports the existing
  version instead of creating duplicates;
- `policy_versions.policy_document_id` references `policy_documents(id)`
  with `ON DELETE RESTRICT`;
- `policy_documents.current_accepted_version_id`, when present, must
  reference an existing `policy_versions(id)` for the same document.

If the same-document current-version rule cannot be represented as a
single SQLite foreign key, the repository must enforce it in the same
transaction that creates or advances the version and tests must cover a
cross-document current-version attempt. Adding a version must be atomic:
insert the immutable version row, advance the document's current version,
and increment the document epoch in one transaction. Concurrent
add-version calls for the same document must produce one monotonically
next version and one deterministic conflict/duplicate outcome; they must
not create two rows with the same version number or leave the document
pointing at an older accepted version.

The compiled JSON projection must be deterministic. Source text and
source hash should both be stored so CLI/API tools can inspect policy
content, deduplicate submissions, and prove exactly what was compiled.
The source hash is computed over the exact UTF-8 source bytes accepted by
the compiler, before any parser normalization. The compiled projection is
serialized with deterministic object-key ordering and stable enum wire
values; warnings persisted in the projection must also be deterministic.

Rejected source should not be persisted by default in Sprint 4. Compile
diagnostics are returned by control-plane use cases and covered by
golden tests. A durable failed-attempt history can be added later as an
explicit audit feature.

Sprint 4 must also close the placeholder left by
`identity_evidence.accepted_policy_id`: accepted identity evidence may
refer only to policy versions, not policy documents or Sprint 3 input
sets. If migration compatibility prevents adding a direct foreign key to
`identity_evidence.accepted_policy_id` in Sprint 4, the design must leave
that column nullable and repositories must reject writes that would store
anything other than a real `PolicyVersionId`. Tests must cover the
negative case where a `PolicyInputSetId` value is supplied as an accepted
policy id.

## 6. Compiled Model

`CompiledPolicy` is a serializable v2-native IR. It is planner-ready
input, not a plan and not a worker contract.

It includes:

- policy name and stable slug;
- source hash;
- language/schema version;
- metadata;
- config;
- phases;
- topologically sorted phase order;
- warnings produced during validation;
- reserved provenance fields for future composition.

Each compiled phase includes:

- name;
- dependency list;
- optional skip condition;
- optional run-if trigger;
- error strategy;
- ordered operations.

Operations are typed enum variants for the accepted Sprint 4 subset:

- set container;
- keep tracks;
- remove tracks;
- reorder tracks;
- set default strategies;
- clear track actions;
- clear tags;
- set tag;
- delete tag;
- conditional block;
- rules block.

Conditions, filters, field paths, values, track targets, comparison
operators, and rule match modes are typed enums rather than raw JSON
where practical. This keeps future planner logic deterministic and
allows compile-time coverage for accepted policy concepts.

## 7. Diagnostics

Diagnostics are first-class product data. A diagnostic contains:

- stable diagnostic code;
- severity: `error` or `warning`;
- stage: `parse`, `validate`, or `compile`;
- byte span;
- line and column;
- message;
- optional suggestion;
- optional related spans.

Public control-plane errors map to existing envelope error codes:

- parse failures use `POLICY_PARSE_ERROR`;
- validation and compile failures use `POLICY_VALIDATION_ERROR`.

Detailed diagnostic codes remain more specific and stable for CLI/API
consumers. Required diagnostic codes include stable variants for:

- unexpected token or malformed syntax;
- source size exceeded;
- unknown top-level block;
- unknown phase statement or operation;
- deferred phase inheritance via `extend`;
- duplicate phase name;
- unknown phase reference;
- self-dependency;
- dependency cycle;
- invalid `run_if` trigger;
- invalid `on_error` value;
- unsupported container;
- invalid track target;
- invalid default strategy;
- invalid language code;
- invalid core field path;
- invalid rule match mode;
- tag ordering error;
- ambiguous tag operation conflict;
- deferred composition via `extends`;
- deferred execution operations: `transcode`, `synthesize`, and
  `verify`.

Diagnostic golden fixtures should serialize only deterministic fields.
Messages should be human-readable, but tests should assert stable codes
and spans so agent workflows can rely on them.

## 8. Validation

Validation rejects:

- empty policy names;
- source files larger than 1 MiB;
- duplicate phase names;
- unknown phase references in `depends_on` and `run_if`;
- phase self-dependencies;
- dependency cycles;
- invalid `run_if` trigger values;
- invalid `on_error` values;
- unknown or unsupported container names;
- invalid track targets;
- invalid default strategies;
- invalid language codes in config and language filters;
- unknown core field roots or core field paths;
- invalid `rules` match modes;
- `set_tag` before `clear_tags` in the same phase;
- conflicting tag operations in the same phase when the final result is
  ambiguous;
- unknown top-level blocks, unknown phase statements, and unknown
  operation names not listed in the Sprint 4 accepted subset;
- phase inheritance through `extend`;
- policy composition through `extends`.

Validation warns, but does not reject, for:

- unknown namespaces below reserved external or plugin roots such as
  `plugin.radarr.title`;
- metadata `requires_tools` entries that are not yet represented as
  worker capabilities.

Warnings should be included in compile results and persisted with the
compiled projection for accepted versions.

## 9. V1 Compatibility Position

The v1 DSL is the language backbone, not a runtime compatibility
promise. Sprint 4 should document a "v2 Sprint 4 accepted subset" and
reject or defer unsupported forms clearly.

Valid fixtures should be adapted from v1:

- `minimal.voom`;
- `container-metadata.voom`;
- a reduced `production-normalize.voom` that avoids `transcode`,
  `synthesize`, and `verify`.

Invalid fixtures should cover the diagnostic matrix in this design.

The implementation should not copy v1's in-process evaluator or plugin
runtime assumptions. In v2, policies compile in the control plane. Later
planner/orchestrator work reads accepted compiled policy versions from
SQLite, combines them with current policy inputs and media snapshots,
and emits durable jobs and tickets. Events continue to record facts; they
do not route policy work.

## 10. Testing

Sprint 4 verification includes:

- `voom-policy` parser unit tests for policy, metadata, config, phase,
  operation, condition, and filter syntax;
- validator unit tests for phase references, cycles, invalid enum
  values, language codes, core field paths, extension field warnings,
  rule match modes, tag ordering, and source-size limit;
- compiler tests for normalized compiled IR and phase order;
- golden valid policy fixture tests;
- golden invalid diagnostic fixture tests;
- deterministic compiled JSON projection tests;
- `voom-store` migration inventory test including migration 0007;
- policy registry repository tests for create, get, list, add-version,
  immutability, source hash, duplicate-source handling, same-document
  current-version integrity, version ordering, concurrent add-version
  conflict behavior, and policy id/input-set id separation;
- `voom-control-plane` use-case tests for create, add-version, get,
  list, and compile-without-persist;
- documentation scan for incomplete-work markers;
- `just ci`.

Tests must use the existing sibling unit-test layout. Integration tests
remain under `crates/*/tests/`.

## 11. Acceptance And Traceability Matrix

| Requirement | Sprint 4 coverage | Deferral |
|---|---|---|
| Policy text syntax | `.voom` parser using v1-compatible block syntax for the accepted subset. | Full v1 grammar parity and formatter/fuzzer parity. |
| Policy identity | `policy_documents` and immutable `policy_versions`. | Policy binding to libraries, schedules, or input sets. |
| Stable diagnostics | Structured diagnostics with codes, stages, severities, spans, messages, and suggestions. | Durable failed-attempt history. |
| Compiled policy model | Typed v2 IR for metadata, config, phases, conditions, filters, and accepted operations. | Plan DAG generation and worker payloads. |
| Deterministic persistence | Unique slugs, unique per-document version numbers, duplicate-source handling, atomic current-version advancement, and exact-byte source hashing. | Cross-node policy-authoring conflict resolution beyond deterministic duplicate/conflict errors. |
| V1 examples | Golden fixtures adapted from `minimal`, `container-metadata`, and reduced `production-normalize`. | Runtime equivalence with v1 evaluator. |
| Composition | `extends` recognized and rejected with stable deferred diagnostic. | Bundled parent policies and `file://` loading. |
| External/plugin fields | Reserved extension roots parse, unknown extension namespaces warn, and unknown core-looking roots reject. | External-system registry validation and plugin-defined operation schemas. |
| CLI/API readiness | Control-plane use cases and durable data shape support future surfaces. | User-facing policy commands and UI editing. |
| Control-plane invariants | Policies compile and persist in control-plane/store boundaries; workers do not read `.voom` source. | Orchestrator use of compiled policies. |

## 12. Implementation Order

1. Add `PolicyDocumentId` and `PolicyVersionId` types in `voom-core`.
2. Add `voom-policy` AST, spans, diagnostics, and parser for the
   accepted subset.
3. Add validator and diagnostic golden fixtures.
4. Add compiled model and compiler tests.
5. Add valid fixture files and deterministic compiled JSON projections.
6. Add migration `0007_policy_registry.sql` and embed it in the
   migrator.
7. Add `voom-store::repo::policies`.
8. Add control-plane policy use cases.
9. Add closeout traceability notes if implementation discovers a
   legitimate deferral.

## 13. Open Decisions

No open product decisions remain for Sprint 4. Exact module names,
parser implementation library, diagnostic code spelling, and fixture
paths are implementation details as long as they preserve the boundaries
and acceptance criteria above.
