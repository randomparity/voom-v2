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
(`0001`–`0030`). Read sites are in `crates/voom-store/src/repo/` (store layer)
and `crates/voom-control-plane/src/` (typed higher layer for T-upstream columns).

Scalar typed columns (`INTEGER`/`TEXT` read into a Rust field, not JSON) carry no
field-drop surface and so get no Class-T row of their own. They still reach this
contract when the same value is a field of a typed root above — see the
`TranscodeVideoProfile` note under "Durable typed column changes" below.

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
| worker_capabilities.extra (`$.accelerator` only) | T-upstream | store: repo/execution/workers.rs (`JsonValue`); typed: video_hardware.rs `from_value` → `NvidiaVideoAcceleratorDescriptor` | `NvidiaVideoAcceleratorDescriptor`; `VideoAcceleratorDescriptor` (`backend`-tagged, VAAPI only) | `VaapiVideoAcceleratorDescriptor` | worker-protocol/src/video_acceleration.rs | complete: both descriptor structs reject unknown fields; NVIDIA is stored untagged so pre-#409 rows keep parsing, VAAPI is stored tagged (#409) |
| tickets.payload | T-upstream | store: repo/execution/tickets.rs:532 (`JsonValue`); typed: ticket_payload.rs:83 `from_value` → `WorkflowTicketPayload` | `WorkflowTicketPayload` | `EffectiveTiming` (named struct); `OperationKind` (unit enum — no surface) | ticket_payload.rs, timing.rs | add attr+tests to `WorkflowTicketPayload` and `EffectiveTiming` (Task 4) |
| tickets.result | T-upstream | store: repo/execution/tickets.rs (`JsonValue`); typed: compliance.rs `decode_compliance_extract_result`, workflow ticket-result normalization, and finalize.rs policy-verification adoption | `ExecuteExtractAudioOutputReport`; `ComplianceLegacyAudioExtractResult`; `PolicyVerificationTicketResult` | — (historical `commit_recovery_required` is intentionally opaque `JsonValue`; `PolicyVerificationTicketStatus` is a unit enum) | audio/mod.rs, cases/policy/compliance.rs, workflow/ticket_results.rs | complete: published and complete historical scalar wire forms reject unknown fields; pre-#337 audio serialization compatibility remains; policy-verification results retain their initial durable shape and reject unknown fields (#334) |
| policy_versions.compiled_json | T-upstream | store: repo/policy/policies.rs:483 (`JsonValue`); typed: plans.rs:291 `deserialize_stored_compiled_policy` → `CompiledPolicy`, used by accepted-version planning and compliance execution | `CompiledPolicy` | all distinct content structs for `CompiledOperation`, `TrackFilter`, `CompiledCondition`, and `CompiledValue`; `CompiledConfig`; `CompiledPhase`; `CompiledRunIfWire`; `CompiledRule`; `PolicyProvenance`; diagnostic/span structs; `VideoProfileSettings`; `TranscodeVideoProfile` | compile/compiled.rs, data/video_profile.rs, diagnostic.rs, syntax/span.rs, voom-core/src/media/transcode_video_profile.rs | complete: all 41 tagged variants retain their exact wire shape and reject unknown fields; current and historical compiled versions remain readable (#344) |

### Durable typed column changes

New or retyped durable columns whose value is also a field of a Class-T /
T-upstream typed root, newest first. Each row names the migration, the columns,
and the already-scoped defining file — the scope list needs a new line only when
the change introduces a *new* defining file.

| migration | durable columns | typed root | evolution | defining file |
|---|---|---|---|---|
| `0030` (#409) | `video_profiles.qp` (new, nullable); `video_profiles.preset` (now nullable) | `TranscodeVideoProfile`, reachable from `policy_versions.compiled_json` via `VideoProfileRef` | `qp: Option<u8>` is additive — an older compiled payload without it reads as `None`. `preset: String` → `Option<String>` is a **retype**, so a payload written with `preset` absent is unreadable by a pre-#409 binary: binary-before-DB upgrade ordering applies (ADR 0013). Both fields are `skip_serializing_if = "Option::is_none"`, so every software and NVENC payload is byte-identical to before. | `crates/voom-core/src/media/transcode_video_profile.rs` (already in scope) |
| `0029` (#400) | `video_profiles.cq`, `video_profiles.decode_backend` | same | `cq: Option<u8>` and `decode: VideoDecodeMode` are additive; `decode` defaults to `Software` and is skipped when software. | `crates/voom-core/src/media/transcode_video_profile.rs` (already in scope) |

#### Non-durable coordinated retype: `LocalWorkerBound.accelerator` (#409)

`LocalWorkerBound` is the local worker's stdout readiness handshake, not a
durable column, so it gets no row above. It is recorded here because #409 changes
it non-additively: `accelerator` goes from
`Option<NvidiaVideoAcceleratorDescriptor>` to `Option<VideoAcceleratorDescriptor>`,
a `backend`-tagged enum over the NVIDIA and VAAPI descriptor structs. An NVIDIA
descriptor is therefore nested under `{"backend":"nvidia", …}` on the wire, so a
control plane and a bundled worker binary on opposite sides of the change cannot
exchange a bound payload. ADR 0013's binary-before-DB ordering applies: the pair
is lock-stepped (ADR 0002/0016) and must be deployed together, never mixed.

The NVIDIA durable side is deliberately unchanged. `worker_capabilities.extra`'s
`accelerator` object still holds the **untagged** `NvidiaVideoAcceleratorDescriptor`
for NVIDIA — `local_worker.rs` serializes the inner struct, not the enum — so
pre-#409 rows keep parsing byte-for-byte. Pinned by
`local_worker_test.rs::nvidia_capability_records_the_untagged_descriptor_token_and_capacity`
and by the byte-for-byte payload pin in
`video_acceleration_test.rs::software_and_nvidia_payloads_are_byte_for_byte_unchanged`.

**Reconciliation closed (#409).** `worker_capabilities.extra` was classified
Class P ("passthrough `JsonValue` — no typed read") while `extra.accelerator` was
in fact read typed at `crates/voom-control-plane/src/video_hardware.rs`
(`from_value` → `NvidiaVideoAcceleratorDescriptor`). #409 makes a second
descriptor durable there, so the column is **promoted P → T-upstream**: its typed
surface is the accelerator descriptor structs in
`crates/voom-worker-protocol/src/video_acceleration.rs`, which is in
`scripts/payload-contract-scope.txt`. The rest of the column stays passthrough —
`endpoint` and `secret` are read as untyped strings.

The stored `accelerator` object is **untagged for NVIDIA and `backend`-tagged for
VAAPI**, and a reader tells them apart by the presence of `backend`. The asymmetry
is deliberate: pre-#409 NVIDIA rows are durable and untagged, so tagging NVIDIA
retroactively would make them unreadable, while an untagged VAAPI object would be
indistinguishable from a malformed NVIDIA one.

The capacity SQL no longer needs to tolerate either shape. It read the grouping
key from `json_extract(extra, '$.accelerator.hardware_token')`, a field only the
NVIDIA descriptor carries, so a VAAPI descriptor silently produced no capacity row
and the bound device never received work. #409 changes those three queries in
`crates/voom-store/src/repo/execution/workers.rs` to group on
`json_extract(hardware, '$[0]')` — the capability's own token column, which is
where ADR 0049 §4 puts the stable token and what ADR 0049 §6 defines capacity
across. `max_sessions` is still read from the descriptor, because both descriptors
carry it. A capability with an accelerator descriptor and no `hardware` token is
now a loud `CONFIG_INVALID` rather than a silent zero. Pinned by
`local_worker_test.rs::vaapi_capability_records_the_tagged_descriptor_token_and_capacity`.

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
- `PolicyVerificationTicketResult` (ticket_results.rs) is a terminal named-field
  root, **in scope**. `PolicyVerificationTicketStatus` is a unit enum with no
  field-drop surface.
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
- `crates/voom-control-plane/src/workflow/ticket_results.rs` → complete in #334
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

worker_grants.max_parallel;
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
