# Durable Payload Contract Inventory

This inventory is the completeness record for the durable JSON evolution contract in
ADR 0013. It covers the SQLite schema through migration `0036` and separates payloads by
their current enforcement state.

A durable JSON value is in one of three states:

1. **Enforced typed root.** Production code deserializes the value into a Rust type. Every
   reachable named-field `Deserialize` struct rejects unknown fields, tagged enums use
   newtype variants, and a behavioral test proves rejection.
2. **Safe by construction.** Production code deserializes the value, but its complete shape
   contains only scalar elements, ID newtypes, tuple newtypes, or unit enums. There is no
   named-field surface on which serde could silently drop a field.
3. **Passthrough JSON.** Production code retains the value as `serde_json::Value`; no typed
   deserialization boundary exists.

The guard in `scripts/check-payload-deny-unknown.sh` scans the defining files listed in
`scripts/payload-contract-scope.txt`. A new typed durable root must be added to both this
inventory and that scope file.

## Enforced typed roots

| Durable column | Read boundary | Typed root and closure | Current contract |
|---|---|---|---|
| `events.payload` | `voom-store/src/repo/audit/events.rs` | `Event` and the content structs in all eleven `voom-events/src/payload/*.rs` families | Event content structs reject unknown fields. The tagged `Event` enum uses newtype variants. |
| `commit_intents.target` | `commit_safety_gate/codecs.rs::decode_target` | `CommitTargetWire`, its three variant content structs, and `FileLocationProposalWire` | Wire kinds are `delete_file_location`, `replace_file_location`, and `move_file_location`; every content struct is strict. |
| `commit_intents.closure_initial`, `commit_intents.closure_authorized` | `commit_safety_gate/codecs.rs::decode_closure` | `AffectedScopeClosureWire`, `ClosureWarningWire` | The complete commit-gate closure rejects unknown fields. |
| `commit_intents.override_token` | `commit_safety_gate/codecs.rs::decode_force_path_token` | `ForcePathToken` | The token is strict; `BypassKind` is a unit enum with no field-drop surface. |
| `artifact_commit_records.report` at `$.rooted_target` | `voom-store/src/repo/media/artifacts/commits.rs::pending_commit_target` and `voom-control-plane/src/artifact/commit/mod.rs::rooted_target_from_commit_report` | Both `PersistedRootedTarget` read-boundary structs | The persisted storage-root ID and provider-relative locator reject unknown fields at both commit finalization and recovery boundaries. Other report keys remain passthrough. |
| `worker_capabilities.extra` at `$.accelerator` | `video_hardware.rs` | `VideoAcceleratorDescriptor` and the NVIDIA, VAAPI, and VideoToolbox descriptor closure | Every stored descriptor is `backend` tagged and every descriptor struct is strict. Other keys in `extra` remain passthrough. |
| `tickets.payload` | `ticket_payload.rs::parse_ticket` | `WorkflowTicketPayload`, `EffectiveTiming` | The workflow envelope and timing fields are strict. `rendered_payload` and `source_file` remain intentionally opaque inside that envelope. |
| `tickets.result` | compliance and workflow ticket-result decoders | `ExecuteExtractAudioOutputReport`, `ComplianceLegacyAudioExtractResult`, `PolicyVerificationTicketResult` | Published and historical typed result forms reject unknown fields. Recovery-only subpayloads remain opaque. |
| `audio_extract_operations.worker_result` | staged extraction recovery in `audio/mod.rs` | `ExtractAudioResult` and its output, stream, and observed-facts closure | Stored worker results use the worker-protocol wire contract and reject unknown fields throughout the closure. |
| `audio_synthesis_operations.worker_result` | staged synthesis validation and recovery in `audio/mod.rs` | `TranscodeAudioResult` and its stream, disposition, and observed-facts closure | Stored worker results use the worker-protocol wire contract and reject unknown fields throughout the closure. |
| `audio_extract_operation_outputs.result_facts` | `audio/mod.rs::recovery_output_input` | `AudioObservedFacts` | Recovery facts reject unknown fields before they can drive a resumed commit. |
| `policy_versions.compiled_json` | `plans.rs::deserialize_stored_compiled_policy` | `CompiledPolicy` and its compiled operation, filter, condition, value, profile, diagnostic, span, and provenance closure | All 41 tagged variants use strict content structs. Intentionally opaque metadata and provenance maps remain `JsonValue`. |
| `remote_idempotency_keys.response_json` | `remote_idempotency.rs::reserve_or_replay_in_tx`, then route-specific replay decoders | `RemoteMutationReplay`, `RemoteAcquireOutcome`, execution heartbeat/complete/fail outcomes, scan start/batch/failure outcomes, `RemoteLeaseDispatch`, and `RemoteArtifactAccessPlan` | Envelope status values are `ok` and `error`; acquire outcomes are `idle`, `no_candidate`, and `leased`. Scan mutation routes decode stored `data` into their route-specific strict outcome. Strict wire structs preserve the public domain enums while rejecting unknown fields. |

### Event families and audit boundary

`SqliteEventRepo::append_in_tx` is the single durable event emission path. Reads reconstruct the
tagged `Event` value in `repo/audit/events.rs` before deserializing it. The strict content structs
therefore protect the audit log at both the emission taxonomy and historical-read boundary.

The enforced families are:

- artifact lifecycle;
- commit lifecycle;
- execution lifecycle;
- external systems;
- media identity;
- policy;
- scan sessions;
- storage roots;
- system;
- use leases;
- workers.

The enum itself is adjacently tagged. Strictness belongs on each newtype variant's content struct,
where serde enforces it, rather than on the tagged enum.

#### Event payload domain typing

Event payloads carry the existing `voom-core` ID newtypes wherever the field identifies one
durable entity. The serde-transparent IDs preserve the historical JSON number representation:

- execution events use `JobId`, `TicketId`, `LeaseId`, and `WorkerId`;
- worker events use `NodeId`, `NodeIncarnationId`, and `WorkerId`; incarnation lifecycle
  status and reason fields use their closed `voom-core` vocabularies;
- commit events use `CommitId`, `EvidenceId`, and `UseLeaseId`, including typed
  `fresh_lease_ids` vectors on post-mutation and recovery-required events;
- media-identity events use the matching work, variant, bundle, file, evidence, snapshot, and
  worker IDs;
- artifact events use the matching execution, file, snapshot, bundle, handle, location,
  verification, commit-record, worker, and use-lease IDs;
- use-lease events use `UseLeaseId` and `FileLocationId`;
- external-system events use `ExternalSystemId` and `ExternalSystemLinkId`.
- scan-session events use `ScanSessionId`, `StorageRootId`, and `ScanSessionStatus`.

`IdentityEvidenceRecordedPayload.assertion_type` uses `AssertionKind`. Its canonical JSON remains
the existing snake-case token, and deserialization now rejects tokens outside that complete
vocabulary.

The following event fields intentionally remain primitive:

- `target_id` and `scope_id` are polymorphic; their companion type field determines the entity;
- capability, grant, artifact-lineage, bundle-member, lineage, and dispatch-attempt IDs have no
  matching `voom-core` newtype;
- provider names and versions, external references, reason and error text, job kinds, artifact
  lineage operations, and status values without a shared complete enum remain open strings.

These source-level type changes require no historical-data migration: canonical serialization is
unchanged, and the strict content structs continue to reject unknown fields.

### Commit-gate payloads

The commit gate persists the proposed target, affected-scope closure, authorization closure,
override token, target epochs, and accepted evidence IDs. The target, closure, and override are
strict typed roots. Target epochs and evidence IDs are safe by construction and are listed below.

The target wire names and the `BypassKind` values are durable vocabulary. Renaming one is a
coordinated wire change, not a source-only refactor.

### Workflow and remote-execution payloads

Workflow ticket payloads enforce their routing envelope while keeping worker-specific rendered
payloads opaque. Ticket results enforce the result forms that the control plane reads back.

Remote idempotency records have two typed layers. `RemoteMutationReplay` enforces the outer
`status` envelope, and the route decoder enforces the `data` value for that route. A replay that
cannot be decoded is repointed to a terminal error so the completed mutation is never executed
again. Migration `0033` adds `scheduler_decision_id: 0` to older acquire outcomes, making all
stored acquire replay data conform to the current typed shape. Scan-session start, observation
batch, and failure routes persist and decode distinct strict outcome structs; their input request
hashes and observation details are not copied into replay responses.

### Policy payloads

`CompiledPolicy` is the root of the largest typed closure. Its tagged enums use distinct strict
content structs, including all compiled operations, track filters, conditions, and values.
`VideoProfileRef` retains its compatibility visitor; inline profiles delegate to strict
`VideoProfileSettings`, and named profiles resolve to strict `TranscodeVideoProfile` values.

## Durable wire evolution

Migration identifiers identify the durable schema transitions that make the current reader safe.

| Migration | Durable change | Current read contract |
|---|---|---|
| `0033` | Adds a missing `scheduler_decision_id` to historical remote acquire replay outcomes. | `RemoteAcquireOutcome` always receives the field; `0` identifies a replay written before scheduler-decision persistence. |
| `0032` | Adds nullable `video_profiles.qp` and makes `video_profiles.preset` nullable. | `qp: Option<u8>` is additive. The `preset` retype requires binary-before-database upgrade ordering. Absent optional fields remain omitted from canonical inline-profile identity input. |
| `0031` | Adds `backend: "nvidia"` to stored accelerator descriptors that predate backend-neutral acceleration. | `VideoAcceleratorDescriptor` can deserialize every stored accelerator without shape sniffing. |
| `0030` | Adds nullable `video_profiles.bitrate_kbps` and an accelerator-claim backend. | `bitrate_kbps: Option<u32>` is additive and affects inline-profile identity only when present. |
| `0029` | Adds `video_profiles.cq` and `video_profiles.decode_backend`. | `cq: Option<u8>` is additive; absent decode mode means software and is omitted when serialized. |

`LocalWorkerBound.accelerator` is not durable, but it shares the backend-tagged
`VideoAcceleratorDescriptor` contract with stored worker capabilities. The control plane and bundled
worker are version-locked under ADRs 0002 and 0016, so a non-additive readiness-handshake change is
deployed as one binary set.

Accelerator capacity groups by the capability's stable token in `hardware[0]`. The descriptor
supplies `max_sessions`. A descriptor without a hardware token is `CONFIG_INVALID`; silently
treating it as zero capacity would hide a broken worker configuration.

## Safe by construction

These typed JSON values contain no reachable named-field struct and therefore need no guard entry:

- `commit_intents.target_row_epochs` → `Vec<TargetRowEpochTriple>`, where the element is a tuple
  newtype;
- `commit_intents.accepted_evidence_ids` → `Vec<EvidenceId>`, where the element is an ID newtype;
- `OperationKind` and `BypassKind` values → unit enums;
- string and integer arrays in worker capabilities, worker grants, workflow summaries, and policy
  inputs → `Vec<String>` or `Vec<u64>`;
- `library_roots.include_globs`, `library_roots.exclude_globs`, and
  `library_roots.extension_allowlist` → `Vec<String>`;
- `artifact_handles.allowed_access_modes`, `artifact_access_plans.input_handles`, and
  `artifact_access_plans.output_handles` → `Vec<String>`.

## Guard scope

The scope file groups defining sources for these enforced closures:

- all eleven event payload families and their `Event` root, including scan-session and
  storage-root lifecycle facts;
- commit-gate target, closure, and override wire types;
- artifact-commit rooted targets at the store and control-plane read boundaries;
- workflow ticket payloads, timing, and ticket results;
- remote idempotency envelopes and route-specific replay outcomes;
- durable audio worker results, recovery facts, and policy-verification result roots;
- compiled policy, video-profile, diagnostic, span, and provenance types;
- backend-neutral accelerator descriptors.

The guard rejects a named-field `Deserialize` struct without
`#[serde(deny_unknown_fields)]` and a tagged enum with inline struct variants. Behavioral tests
independently verify that unknown fields fail deserialization at each root and nested closure.

## Passthrough JSON

The following columns remain `JsonValue` at production read boundaries:

- `worker_grants.max_parallel`;
- `artifact_handles.source_lineage`;
- `artifact_commit_records.report` except `$.rooted_target`, and
  `artifact_verifications.report`;
- `audio_extract_operation_outputs.probe_payload`;
- `audio_synthesis_operations.probe_payload`;
- `audio_synthesis_companions.result_facts`;
- `external_systems.connection_profile`, `external_systems.rate_limit_config`;
- `quality_scoring_profiles.definition`, `quality_scores.dimension_scores`,
  `quality_scores.provenance`;
- `artifact_access_plans.evidence`;
- `workflow_summaries.per_operation`, `workflow_phase_summaries.report`,
  `scheduler_decisions.explanation_json`;
- `nodes.metadata`, `identity_evidence.provenance`, `media_snapshots.payload`;
- `identity_evidence.pinned_file_version_ids`, `identity_evidence.pinned_hashes`,
  `identity_evidence.pinned_locations`;
- `policy_media_snapshot_inputs.stream_summary`;
- `policy_identity_evidence_inputs.provenance`;
- `policy_bundle_target_inputs.artifact_expectation`;
- `policy_quality_profile_selections.dimension_weights`;
- `policy_issue_inputs.provenance`.

If production code starts deserializing one of these values into a typed root, move it to the
enforced or safe-by-construction section and update `scripts/payload-contract-scope.txt` when a new
defining file enters the strict closure.
