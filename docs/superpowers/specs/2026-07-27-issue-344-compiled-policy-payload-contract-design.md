# Issue #344: Compiled-policy durable payload contract design

## Goal

Bring `policy_versions.compiled_json` under ADR 0013 with structurally enforced
unknown-field rejection at the root and every reachable named-field layer.
Preserve the published DSL, every existing compiled JSON shape, and all
historical reads.

## Scope and accepted source break

This change owns:

- reclassifying `policy_versions.compiled_json` as T-upstream;
- inventory and guard scope for its complete typed serde graph;
- strict root, nested-struct, and tagged-variant deserialization;
- all internal workspace constructor and pattern updates;
- stored planning and compliance failure ordering;
- current-wire and historical-read compatibility evidence; and
- compiled-policy upgrade and rollback guidance.

It explicitly accepts an internal Rust source break. Four public enums change
from inline struct or unit variants to newtype variants over public content
structs. Workspace callers must use tuple-newtype construction and matching.
There is no compatibility shim, dual API, or deprecated shape.

It does not add parser or DSL forms, alter a compiled JSON field or tag, add a
migration or dependency, or change policy semantics. Phase-source spelling is
owned by #346, track-filter escaping by #358, and executable rollback health
procedure by #351.

## Stored read and write boundaries

`voom-store::PolicyRepo` writes
`deterministic_json(&CompiledPolicy)` to `policy_versions.compiled_json` and
reads the column as `serde_json::Value`.

The sole typed stored-read boundary is
`cases::policy::plans::deserialize_stored_compiled_policy`. Planning and
compliance both call it. The inventory records this shared highest-layer
boundary and both consumers.

The helper:

1. applies the bounded raw stream-condition compatibility gate;
2. deserializes `CompiledPolicy`;
3. compares typed source hash and schema version with the row;
4. applies typed stream-condition eligibility; and
5. materializes legacy execution defaults in memory.

No other `policy_versions.compiled_json -> CompiledPolicy` parse exists.

## Reachable deserialization graph

- `voom-policy/src/compile/compiled.rs`: `CompiledPolicy`, `CompiledConfig`,
  `PolicyProvenance`, `CompiledPhase`, `CompiledRunIfWire`, 41 tagged-variant
  content structs, `CompiledRule`, and their scalar/unit enums.
- `voom-policy/src/data/video_profile.rs`: `VideoProfileRef` and
  `VideoProfileSettings`.
- `voom-policy/src/diagnostic.rs`: `PolicyDiagnostic` and `RelatedSpan`.
- `voom-policy/src/syntax/span.rs`: `SourceSpan` and `SourceLocation`.
- `voom-core/src/media/transcode_video_profile.rs`:
  `TranscodeVideoProfile`, reachable through `resolved_profile`.

`BTreeMap<String, Value>` metadata and provenance flags are intentional opaque
leaves. Arbitrary keys are their data, not dropped struct fields. Scalars,
collections of scalars, newtype IDs, and unit enums have no named-field drop
surface.

## Public tagged enum representation

ADR 0013 requires tagged variants to be newtypes over separately annotated
content structs. Serde flattens a struct newtype into the same internally tagged
JSON object, so content type names never enter the wire.

Every variant gets a distinct public content struct. Even identical current
field sets are not shared: future additive evolution of one variant must not
silently widen another variant's accepted wire shape.

### `CompiledOperation`

- `SetContainer` uses `CompiledSetContainerOperation` with `container`.
- `KeepTracks` uses `CompiledKeepTracksOperation` with `target` and `filter`.
- `RemoveTracks` uses `CompiledRemoveTracksOperation` with `target` and `filter`.
- `ReorderTracks` uses `CompiledReorderTracksOperation` with `targets` and
  `head_filter`.
- `SetDefaults` uses `CompiledSetDefaultsOperation` with `target`, `strategy`,
  and `filter`.
- `ClearTrackActions` uses `CompiledClearTrackActionsOperation` with `target`.
- `ClearTags` uses fieldless `CompiledClearTagsOperation`.
- `SetTag` uses `CompiledSetTagOperation` with `key` and `value`.
- `DeleteTag` uses `CompiledDeleteTagOperation` with `key`.
- `TranscodeVideo` uses `CompiledTranscodeVideoOperation` with `target_codec`,
  `container`, `profile`, and `resolved_profile`.
- `TranscodeAudio` uses `CompiledTranscodeAudioOperation` with `target_codec`,
  `container`, and `filter`.
- `ExtractAudio` uses `CompiledExtractAudioOperation` with `target_codec`,
  `container`, and `filter`.
- `SynthesizeAudio` uses `CompiledSynthesizeAudioOperation` with `target_codec`,
  `container`, `target_channels`, and `filter`.
- `VerifyArtifact` uses fieldless `CompiledVerifyArtifactOperation`.
- `Conditional` uses `CompiledConditionalOperation` with `condition` and
  `operations`.
- `Rules` uses `CompiledRulesOperation` with `mode` and `rules`.

### `TrackFilter`

| Variant | Content struct | Fields |
|---|---|---|
| `LanguageIn` | `LanguageInTrackFilter` | `values` |
| `CodecIn` | `CodecInTrackFilter` | `values` |
| `Channels` | `ChannelsTrackFilter` | `op`, `value` |
| `Commentary` | `CommentaryTrackFilter` | none |
| `Forced` | `ForcedTrackFilter` | none |
| `Default` | `DefaultTrackFilter` | none |
| `Font` | `FontTrackFilter` | none |
| `TitleContains` | `TitleContainsTrackFilter` | `value` |
| `TitleMatches` | `TitleMatchesTrackFilter` | `value` |
| `Not` | `NotTrackFilter` | `inner` |
| `And` | `AndTrackFilter` | `filters` |
| `Or` | `OrTrackFilter` | `filters` |

### `CompiledCondition`

| Variant | Content struct | Fields |
|---|---|---|
| `Exists` | `CompiledExistsCondition` | `target`, `filter` |
| `Count` | `CompiledCountCondition` | `target`, `op`, `value` |
| `FieldComparison` | `CompiledFieldComparisonCondition` | `path`, `op`, `value` |
| `FieldExists` | `CompiledFieldExistsCondition` | `path` |
| `Predicate` | `CompiledPredicateCondition` | `name` |
| `Not` | `CompiledNotCondition` | `inner` |
| `And` | `CompiledAndCondition` | `conditions` |
| `Or` | `CompiledOrCondition` | `conditions` |

### `CompiledValue`

| Variant | Content struct | Fields |
|---|---|---|
| `String` | `CompiledStringValue` | `value` |
| `Number` | `CompiledNumberValue` | `value` |
| `Boolean` | `CompiledBooleanValue` | `value` |
| `FieldPath` | `CompiledFieldPathValue` | `path` |
| `List` | `CompiledListValue` | `values` |

Every content struct derives `Serialize` and `Deserialize` and carries
`#[serde(deny_unknown_fields)]`. Field serde attributes move unchanged from the
inline variant to its content field. Empty braced structs make fieldless
variants reject sibling fields while preserving JSON such as
`{"type":"clear_tags"}`.

The three additive omission fields keep their exact attributes:

- `CompiledReorderTracksOperation.head_filter` remains
  `default` plus `skip_serializing_if = "Option::is_none"`;
- `CompiledSetDefaultsOperation.filter` remains
  `default` plus `skip_serializing_if = "Option::is_none"`; and
- `CompiledTranscodeVideoOperation.resolved_profile` remains
  `default` plus `skip_serializing_if = "Option::is_none"`.

All other fields retain their current serde-required and serialization behavior.
In particular, other `Option` fields continue to serialize `null` rather than
being omitted.

The public enum names and variant names remain unchanged. Content structs are
public under `voom_policy::compiled` because downstream workspace crates must
construct and inspect them. They are not re-exported from the crate root.

## Ordinary structs and compatibility readers

`CompiledPolicy`, `PolicyProvenance`, `CompiledPhase`, `CompiledRule`,
`PolicyDiagnostic`, `RelatedSpan`, `SourceSpan`, and `SourceLocation` receive
`deny_unknown_fields`.

`CompiledConfig` changes from a manual root deserializer to a normal derived,
strict struct. Its `languages` and `on_error` fields use compatibility-aware
field deserializers:

- absent fields keep their current defaults;
- current typed values remain unchanged;
- historical statement strings remain readable;
- legacy `Skip` remains readable; and
- a present null or malformed value retains current rejection behavior.

This removes the manual root implementation and places the config under the
existing structural guard.

`CompiledRunIf` continues to derive through strict `CompiledRunIfWire`. The
public adapter uses the guard's existing justified exemption convention because
the wire struct is the effective serde unit. A behavioral test injects an
unknown field.

`VideoProfileRef` is the one pre-existing manual compatibility reader that
remains. It must accept both a legacy bare string and current externally tagged
objects. Its visitor already rejects unknown tags, empty objects, trailing
keys, and unknown inline settings. The inventory records this narrow audited
exception, and direct behavioral tests cover every rejection path. The guard
still enforces strict `VideoProfileSettings`. No guard marker or macro-analysis
feature is added.

## Failure behavior and durable ordering

Direct serde vectors cover every one of the 41 variants. Each vector:

1. deserializes exact current JSON to the expected public value;
2. serializes the public value back to the exact JSON;
3. injects an unknown sibling field; and
4. requires visible rejection.

The vectors include optional fields both absent and present, recursive values,
fieldless variants, and same-typed multi-field content.

Additional tests inject unknown fields at the `CompiledPolicy` root and every
ordinary reachable named struct.

A stored planning test corrupts an accepted policy row with an unknown nested
field and invokes `plan_accepted_policy_version_with_input_set`. It requires
contextual `PLAN_GENERATION_ERROR` before any durable work state.

A mutating compliance test first seeds a valid matching issue and event, then
corrupts the stored policy and invokes the test-runtime execution entry point.
It requires:

- the same contextual error;
- `partial: None`;
- identical complete issue, event, and policy-input state;
- no jobs, tickets, or leases; and
- byte-identical stored `compiled_json` text.

This proves decode remains before issue application and workflow creation.

## Compatibility, upgrade, and rollback

The implementation writes no new JSON field and rewrites no stored row.

Compatibility evidence distinguishes:

- exact JSON equality for current canonical fixtures and all 41 variant vectors;
- unchanged published grammar compiled goldens; and
- semantic readability for intentionally normalizing legacy config strings and
  bare profile strings.

Historical `Skip`, tagged named/inline profiles, `title_matches`, config-less
roots, and checked-in historical fixtures remain readable.

Future compiled-policy changes follow ADR 0013:

- fields are additive and optional/defaulted;
- the reader binary deploys before a writer can persist a new field;
- an older binary intentionally rejects a newer row containing that field; and
- rollback after such a write requires the pre-upgrade database snapshot.

There is no alternate format, payload version shim, or transparent downgrade
claim.

## Alternatives rejected

### Private mirror wire enums

Rejected after review. They preserve the Rust enum API but duplicate all variant
fields and require handwritten conversion coverage. The static guard then needs
new machinery to audit manual enum readers. That design is smaller at call
sites but larger and less reliable at the wire boundary.

### Shared same-shaped content structs

Rejected. They reduce boilerplate but couple future accepted fields across
semantically distinct variants.

### Public compatibility constructors or matching shims

Rejected. Constructors cannot preserve struct-variant pattern syntax, and a
second API would violate replace-not-deprecate.

## Verification

- Record red failures from the expanded production guard scope.
- Record red unknown-field assertions against the permissive baseline.
- Run all per-variant exact-wire and rejection vectors.
- Run ordinary-root and nested strictness tests.
- Run legacy config/profile/Skip/title tests.
- Run every compiled fixture and published grammar golden test.
- Run stored planning and mutating compliance failure tests.
- Run both payload guard checks and their self-test.
- Run focused policy, plan, control-plane, lint, and formatting checks.
- Run `just ci`.
