# Durable Payload Contract Inventory (audit M4, #220)

Completeness artifact for the deny-unknown-fields contract (ADR 0013). Every
durable JSON column is listed exactly once. A Class-T / T-upstream row is "done"
only when its typed root (and reachable named-field sub-structs) carry an
**effective** `#[serde(deny_unknown_fields)]` (per the spec §1 placement rule) and
a behavioral unknown-field-rejection test exists. Class-P rows carry no M4 risk.

The guard (`scripts/check-payload-deny-unknown.sh`) scans exactly the files listed
in `scripts/payload-contract-scope.txt`, which is the set of "defining file(s)"
for every Class-T / T-upstream row below.

Durable columns are surveyed across the SQLite migrations in `migrations/`
(`0001`–`0016`). Read sites are in `crates/voom-store/src/repo/` (store layer)
and `crates/voom-control-plane/src/` (typed higher layer for T-upstream columns).

## Class T / T-upstream (contract applies)

| table.column | class | read site | typed root | reachable typed sub-structs | defining file(s) | action |
|---|---|---|---|---|---|---|
| events.payload | T | repo/audit/events.rs:280 `from_value::<Event>` | `Event` (adjacently tagged, newtype variants — enum itself effective) | all variant content structs in voom-events/src/payload/{artifact,commit,execution,external_system,media_identity,policy,system,use_leases,workers}.rs | those 9 files | artifact.rs is only ~half-covered (16/32) — sweep all 8 files incl. artifact.rs (Task 5) |
| commit_intents.target | T | repo/media/commit_safety_gate/codecs.rs:182 `decode_target` (`let wire: CommitTargetWire = from_str`) | `CommitTargetWire` (internally tagged, **inline** struct-variants) | `FileLocationProposalWire` | codecs.rs | extract variants to newtype content structs; add attr (Task 3) |
| commit_intents.closure_initial / closure_authorized | T | repo/media/commit_safety_gate/codecs.rs:188 `decode_closure`; called from authorize.rs:361 / finalize.rs | `AffectedScopeClosureWire` | `ClosureWarningWire` | codecs.rs | add attr (Task 3) |
| commit_intents.override_token | T | repo/media/commit_safety_gate/codecs.rs:168 `decode_force_path_token` (`from_str` → `ForcePathToken`) | `ForcePathToken` | — (`BypassKind` unit enum) | commit_safety_gate.rs | add attr (Task 3) |
| commit_intents.target_row_epochs | T | repo/media/commit_safety_gate/finalize.rs:393 `decode_target_row_epochs` (`from_str::<Vec<TargetRowEpochTriple>>`) | `TargetRowEpochTriple` (tuple newtype, no named fields) | — | codecs.rs | safe by construction; NOT in scope |
| commit_intents.accepted_evidence_ids | T | repo/media/commit_safety_gate/authorize.rs:362 / abort_list.rs:279 (`Vec<EvidenceId>`) | `EvidenceId` (id newtype) | — | n/a | safe by construction; NOT in scope |
| worker_capabilities.{codecs,hardware,artifact_access}; worker_grants.{can_execute,can_access_read,can_access_write,denies}; workflow_file_phase_summaries.ticket_ids; policy_media_snapshot_inputs.{audio_languages,subtitle_languages,health_flags} | T | executor.rs:1403 `json_string_array_contains` (`Vec<String>`); workflow_summaries.rs:622 (`Vec<u64>`); policy_inputs.rs:813–815 `json_value` → `Vec<String>` | `Vec<String>` / `Vec<u64>` | — | n/a | scalar element types, no named-field surface; NOT in scope |
| tickets.payload | T-upstream | store: repo/execution/tickets.rs:532 (`JsonValue`); typed: ticket_payload.rs:83 `from_value` → `WorkflowTicketPayload` | `WorkflowTicketPayload` | `EffectiveTiming` (named struct); `OperationKind` (unit enum — no surface) | ticket_payload.rs, timing.rs | add attr+tests to `WorkflowTicketPayload` and `EffectiveTiming` (Task 4) |
| tickets.result | T-upstream | store: repo/execution/tickets.rs (`JsonValue`); typed: compliance.rs `decode_compliance_extract_result` and workflow execution ticket-result normalization | `ExecuteExtractAudioOutputReport`; `ComplianceLegacyAudioExtractResult` | — (historical `commit_recovery_required` is intentionally opaque `JsonValue`) | audio/mod.rs, cases/policy/compliance.rs | complete: published and complete historical scalar wire forms reject unknown fields; pre-#337 serialization compatibility and rejection tests present (#337) |
| policy_versions.compiled_json | T-upstream | store: repo/policy/policies.rs:483 (`JsonValue`); typed: plans.rs:291 `deserialize_stored_compiled_policy` → `CompiledPolicy`, used by accepted-version planning and compliance execution | `CompiledPolicy` | all distinct content structs for `CompiledOperation`, `TrackFilter`, `CompiledCondition`, and `CompiledValue`; `CompiledConfig`; `CompiledPhase`; `CompiledRunIfWire`; `CompiledRule`; `PolicyProvenance`; diagnostic/span structs; `VideoProfileSettings`; `TranscodeVideoProfile` | compile/compiled.rs, data/video_profile.rs, diagnostic.rs, syntax/span.rs, voom-core/src/media/transcode_video_profile.rs | complete: all 41 tagged variants retain their exact wire shape and reject unknown fields; current and historical compiled versions remain readable (#344) |

### Transitive typed closure (named-field `Deserialize` sub-structs)

For each Class-T / T-upstream root, the reachable named-field `Deserialize`
structs (the field-drop surface) and the no-surface members:

- `Event` (voom-events) → all per-variant content structs across the 8
  `payload/*.rs` files. Each content struct is a named-field `Deserialize` struct
  and is in scope (Task 5). The enum itself is adjacently tagged with newtype
  variants, so the attribute lands on the content structs, not the enum.
- `CommitTargetWire` (codecs.rs:21) → `FileLocationProposalWire` (codecs.rs:37,
  named-field struct, **in scope**). Variants are currently inline struct-variants
  (`Delete` / `Replace` / `Move`); Task 3 extracts them to newtype content structs.
- `AffectedScopeClosureWire` (codecs.rs:110) → `ClosureWarningWire` (codecs.rs:119,
  named-field struct, **in scope**). Other fields are `BTreeSet`/`Vec` of id
  newtypes (no surface).
- `ForcePathToken` (commit_safety_gate.rs:492) → fields are `String`, `String`,
  `BTreeSet<BypassKind>`; `BypassKind` (commit_safety_gate.rs:482) is a unit-variant
  enum — **no field-drop surface**. The struct itself is in scope (Task 3).
- `TargetRowEpochTriple` (codecs.rs:299) → tuple newtype
  `(TargetMemberKind, u64, u64)`, **no named fields** → attribute inapplicable, safe
  by construction; **NOT in scope**.
- `WorkflowTicketPayload` (ticket_payload.rs:8) → `EffectiveTiming` (timing.rs:4,
  named-field struct, **in scope**); `OperationKind` (voom-core, unit-variant enum —
  no surface); `rendered_payload: Value` and `source_file: Option<Value>` are
  untyped passthrough (not a deserialization boundary, not in scope).
- `EffectiveTiming` (timing.rs:4) → only `u64` scalar fields; terminal, no further
  nested structs.
- `ExecuteExtractAudioOutputReport` and `ComplianceLegacyAudioExtractResult`
  (compliance.rs) are terminal named-field roots, **in scope**. The historical
  recovery subpayload remains intentionally opaque. `ComplianceAudioExtractOutput`
  and `ComplianceLegacyAudioExtractOutput` are Serialize-only report projections,
  not typed durable-read roots.
- `CompiledPolicy` (compiled.rs) → strict ordinary structs and distinct strict
  content structs for all 41 tagged variants. `CompiledRunIf` delegates to the
  strict `CompiledRunIfWire`. `VideoProfileRef` retains its audited compatibility
  visitor, whose inline form delegates to strict `VideoProfileSettings`.
  `TranscodeVideoProfile` is strict. Metadata and provenance flags remain
  intentionally opaque `JsonValue` maps.

**No named fields → attribute inapplicable, safe by construction** (recorded, not in
scope): `TargetRowEpochTriple` (tuple newtype), `EvidenceId` (id newtype),
`OperationKind` (unit enum), `BypassKind` (unit enum).

### Reconciliation (Step 5b)

Every Class-T / T-upstream defining file in scope maps to a sweep task:

- `crates/voom-events/src/payload/*.rs` → Task 5
- `crates/voom-store/src/repo/media/commit_safety_gate/codecs.rs`,
  `…/commit_safety_gate.rs` → Task 3
- `crates/voom-control-plane/src/workflow/plan/ticket_payload.rs`,
  `…/execution/timing.rs` → Task 4
- `crates/voom-control-plane/src/audio/mod.rs`,
  `…/cases/policy/compliance.rs` → complete in #337
- `crates/voom-policy/src/compile/compiled.rs`, `…/data/video_profile.rs`,
  `…/diagnostic.rs`, `…/syntax/span.rs`, and
  `crates/voom-core/src/media/transcode_video_profile.rs` → complete in #344

A full sweep of every non-test typed deserialization read (`from_value::<T>`,
`from_str::<T>`, and type-annotated-let `from_str`/`from_value`) across all crates
surfaced no Class-T / T-upstream root with a named-field surface outside these
files. No new sweep task is required.

```
Reconciliation result: [x] all discovered roots map to Tasks 3–5 (no new task needed)
                       [ ] new sweep task(s) added: ____________________
```

## Class P (passthrough JsonValue — no typed read, no risk)

worker_capabilities.extra; worker_grants.max_parallel;
artifact_handles.{allowed_access_modes,source_lineage};
artifact_commit_records.report; artifact_verifications.report;
audio_extract_operation_outputs.{probe_payload,result_facts}
(in-memory `*Report` derive neither Serialize nor Deserialize);
external_systems.{connection_profile,rate_limit_config};
quality_scoring_profiles.definition; quality_scores.{dimension_scores,provenance};
remote_idempotency_keys.response_json;
artifact_access_plans.{input_handles,output_handles,evidence};
workflow_summaries.per_operation;
workflow_phase_summaries.report; scheduler_decisions.explanation_json;
nodes.metadata; identity_evidence.provenance; media_snapshots.payload;
identity_evidence.{pinned_file_version_ids,pinned_hashes,pinned_locations}
(read in repo/media/identity.rs via `from_str` → `Option<JsonValue>`);
policy_media_snapshot_inputs.stream_summary;
policy_identity_evidence_inputs.provenance;
policy_bundle_target_inputs.artifact_expectation;
policy_quality_profile_selections.dimension_weights;
policy_issue_inputs.provenance
(read in repo/policy/policy_inputs.rs via `json_value` → `JsonValue`).

If a future change starts typing any Class-P column, add it to the Class-T table
above and to `scripts/payload-contract-scope.txt`.
