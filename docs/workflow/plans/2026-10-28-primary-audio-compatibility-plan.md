# Primary audio compatibility implementation plan (#765)

Goal: retain English and unknown audio, ensure one deterministically selected compatibility identity, then explicitly place it first among audio and make it the sole audio default.

Architecture: `voom-policy` owns the two parameter-free operations; `voom-plan` resolves source, target, companion identity and placement. The existing synthesis ledger publishes one owner-node FFmpeg result, and a dependent owner-node MKVToolNix remux places it against refreshed committed facts. Observed media tags retain identities across probes; they do not grant authorization or replace canonical publication lineage.

Stack: existing Rust workspace, serde/serde_json, Tokio, sqlx/SQLite, axum, out-of-process FFmpeg, MKVToolNix and ffprobe. No dependency or tool-version change.

Expected implementation size: 1900–2900 changed lines (L) — 250–350 policy/protocol, 400–600 planner, 200–300 normalization/publication, 250–400 workers, 100–150 binding and 700–1100 focused/generated-media tests and sample updates

Authority: [frozen scope](https://github.com/randomparity/voom-v2/issues/765#issuecomment-6457264164), token `q765-dd13d5e5`; [design](../specs/2026-10-28-primary-audio-compatibility-design.md); [ADR 0111](../../adr/0111-primary-audio-compatibility.md). Full-spec lane; fixed L denominator **1000**, grounded by the live cross-crate assessment in that scope comment. The implementation estimate includes several hundred lines of real-owner acceptance scaffolding; it does not change that denominator or expand scope.

Branch `feat/primary-audio-compatibility-765`; `BASE_BRANCH=main`; inspected base `20cdc73df1ff028c8ed88ad926f3db8543210a4d`. Guardrail: `just ci` before push.

## Global Constraints

The following constraints are transcribed from the design:

> Keep policy vocabulary in `voom-policy`, deterministic resolution in `voom-plan`, SQL/lineage in `voom-store`, authority/publication in the control plane, and byte transformations in owner-node workers. This is a clean extension; no existing responsibility moves and no unrelated path is removed.
>
> Ordinary synthesis remains strictly downmix/non-default.
>
> Optional serialized fields use `#[serde(default)]`; concrete deserialized content structs deny unknown fields. Update durable contract inventory/scope where a typed JSON unit is added. Deploy supporting binaries before admitting policies using new variants/fields. Reverting application code requires ceasing new-policy admission; do not pretend old binaries parse new durable variants.
>
> The existing ledger needs no schema migration; reservation `migrations/0045_*.sql=0045` remains consumed and unused.
>
> Domain IDs stay typed through repositories and publication; persisted corruption remains a database error before domain absence/conflict classification. Use existing named transaction helpers and preserve mutation/event/claim ordering.

Root and crate `AGENTS.md` also bind every task. Read exports, callers and helpers before editing; use sibling `*_test.rs` files for unit tests; use real Tokio time with SQLite and `ManualClock` for domain time. Production SQL belongs in `voom-store`; no new SQL is needed for this feature. Test fixtures may inspect and corrupt storage deliberately. Do not bypass hooks. Preserve existing global remux heads and existing DSL/serialized forms when new fields are absent. No new encoder, generic operation framework, real-library work, instruction-file edit or control-plane byte access.

## Grounded file map and ownership

Paths below are repository-relative. Nothing is moved or removed.

| Files | Current owner and bounded addition | Criteria |
|---|---|---|
| `crates/voom-policy/src/syntax/{parser.rs,parser_test.rs}`, `crates/voom-policy/src/compile/{compiled.rs,compiled_test.rs,validate.rs,validate/operations.rs,validate_test.rs,lower/operations.rs,pipeline_test.rs}` | Existing generic statement parser needs no AST variant; add statement boundary keywords, strict validation/lowering and annotated compiled content structs. | C1–C5 |
| `crates/voom-policy/fixtures/policies/audio-primary-compatibility.voom`, `crates/voom-policy/fixtures/compiled/audio-primary-compatibility.json`, `crates/voom-policy/src/fixtures/policy_fixtures.rs` | Executable sample and deterministic compiled corpus. | C5 |
| **New** `crates/voom-plan/src/planner/audio/{compatibility.rs,compatibility_test.rs}` | Dedicated deterministic resolver and target table; use existing audio/remux owners without weakening their selectors. | C1,C2,C4 |
| `crates/voom-plan/src/planner/audio/{mod.rs,payload.rs,payload_test.rs}`, `crates/voom-plan/src/{planner.rs,planner_test.rs,eligibility.rs,diagnostic.rs,diagnostic_test.rs}` | Ensure payload, status/diagnostics, operation visitors and eligibility. | C1,C2,C4 |
| `crates/voom-plan/src/planner/remux/{mod.rs,payload.rs,payload_test.rs}`, `crates/voom-plan/src/planner_test.rs` | Place resolves a refreshed compatibility identity into remux actions and an audio-group head. | C3,C4 |
| `crates/voom-worker-protocol/src/operations/{audio.rs,audio_test.rs,remux.rs,remux_test.rs}`, `crates/voom-worker-protocol/src/lib.rs` | Strict optional compatibility settings, observed layout, optional audio-group head; old requests stay valid. | C2–C4 |
| `crates/voom-ffprobe-worker/src/{normalize.rs,normalize_test.rs}` | Existing observed tags gain canonical ID/source association and a narrow invalid-language marker. | C1,C4 |
| `crates/voom-control-plane/src/scan/publish.rs` (read-only) | Existing ID fallback already preserves observed IDs; no production change required. | C4 |
| `crates/voom-control-plane/src/audio/{selection.rs,selection_test.rs}`, `crates/voom-control-plane/src/workflow/plan/{binding.rs,binding_test.rs,envelope.rs,envelope_test.rs}` | Recompute pinned ensure selection, carry settings through existing dispatch envelope. | C2,C4 |
| `crates/voom-control-plane/src/remux/{selection.rs,selection_test.rs}` | Validate and bind the new audio-only head to a retained audio stream. | C3,C4 |
| `crates/voom-control-plane/src/artifact/publication/{audio.rs,mod.rs,mod_test.rs}`, **new** `crates/voom-control-plane/src/artifact/publication/audio_test.rs` | Reuse synthesis admission, claim, staged receipt, validation and finalization; validate source/result tags rather than overwrite contradictory observations. | C2,C4 |
| `crates/voom-store/src/repo/media/audio_synthesis_operations_test.rs` | Regress canonical retry/lineage and corruption behavior of existing repository; no schema change or new lookup. | C4 |
| `crates/voom-ffmpeg-worker/src/{ffmpeg.rs,ffmpeg_test.rs,handler.rs,handler_test.rs}` | Compatibility-specific same-channel validation, explicit layout/identity metadata and actual-output validation; ordinary synthesis strict/downmix/non-default. | C2,C4 |
| `crates/voom-mkvtoolnix-worker/src/{mkvmerge.rs,mkvmerge_test.rs,handler_test.rs}` | Permute audio slots after existing ordering; existing handler verifies actual output against the shared order helper. | C3 |
| `crates/voom-node-agent/src/{media.rs,media_test.rs}` | Existing settings/selection cloning already forwards fields; add a focused round-trip test, change production only if required. | C2,C3 |
| **New** `crates/voom-node-agent/tests/primary_audio_compatibility.rs`, **new** `crates/voom-node-agent/tests/support/primary_audio_media.rs` | Real authenticated `AgentRuntime`, bundled subprocess workers, generated MKV, actual committed-byte oracles. | C1–C5 |
| `docs/payload-contract-inventory.md`, `scripts/payload-contract-scope.txt`, `docs/runbooks/operator-real-media-execution.md` | Inventory the actual added typed units; document sample/upgrade ordering in the existing operator runbook. | C5 |

Mechanical constructor migrations accompany the owning task: `crates/voom-worker-protocol/src/operations/dispatch_test.rs`, `crates/voom-fake-support/src/lib_test.rs`, `crates/voom-ffmpeg-worker/tests/transcode_conformance.rs`, existing worker/control-plane test literals. Use `rg -n 'AudioOperationPayload \{|TranscodeAudioSettings \{|AudioOutputStreamFact \{|RemuxOperationPayload \{|RemuxSelection \{' crates` to enumerate exact constructor sites; initialize new optional fields to `None`. No behavior change belongs in these migrations.

Existing protected paths: `synthesize_audio_shape` remains the ordinary strict-downmix gate; `plan_synthesize` and ordinary companion defaults remain unchanged; filter-addressed remux still requires exactly one match. Existing normal Cargo edges remain unchanged. Existing node-agent `media.rs::transcode_audio_payload` clones `dispatch.settings`, and its remux arm clones `dispatch.selection`; neither needs a second interpretation of policy.

## Cast 1 — Strict vocabulary and additive contracts

This is the vocabulary/payload seam used by both ensure and place. It can be reviewed independently from ranking and byte work, but its new operations are not feature-complete until Casts 2–5.

**Interfaces**

- Existing `voom_policy::compile_policy(source: &str)` remains the compiler facade. `CompiledOperation` is the tagged enum in `compile/compiled.rs`.
- Add `CompiledOperation::EnsureAudioCompatibility(CompiledEnsureAudioCompatibilityOperation)` and `CompiledOperation::PlaceAudioCompatibility(CompiledPlaceAudioCompatibilityOperation)`.
- Add `AudioOperationType::EnsureAudioCompatibility`, wire value `ensure_audio_compatibility`, using existing `PlanOperationKind::TranscodeAudio`/`transcode_audio` capability.
- Add `AudioOperationPayload.compatibility: Option<AudioCompatibilityDescriptor>`; add `RemuxOperationPayload.audio_head_snapshot_stream_id: Option<String>` and wire key of the same name.
- Add `TranscodeAudioSettings.compatibility: Option<AudioCompatibilitySettings>`; add `AudioOutputStreamFact.channel_layout: Option<String>`; add `RemuxSelection.audio_head_stream: Option<RemuxStreamRef>`.

**Verification**

| Contract | Mode and case | Expected red | Exact green command |
|---|---|---|---|
| Exact statements and rejection of additional tokens/blocks, top-level and nested | Mode: focused-test; `pipeline_test.rs::compatibility_operations_are_strict` | Unknown operation for valid statements, or an invalid suffix is accepted | `cargo test -p voom-policy compatibility_operations_are_strict` |
| Durable tagged units reject unknown fields | Mode: focused-test; `compiled_test.rs::compatibility_compiled_units_are_strict` | New variants unavailable or extra field accepted | `cargo test -p voom-policy compatibility_compiled_units_are_strict` |
| New wire fields deserialize strictly and absent fields preserve old requests | Mode: focused-test; protocol `audio_test.rs::compatibility_settings_are_additive_and_strict`, `remux_test.rs::audio_head_is_additive_and_strict` | Missing types/fields; malformed content accepted | `cargo test -p voom-worker-protocol compatibility_settings_are_additive_and_strict`; `cargo test -p voom-worker-protocol audio_head_is_additive_and_strict` |
| Payload type/required pins cannot be confused with ordinary synthesis | Mode: focused-test; plan `payload_test.rs::compatibility_payload_requires_pinned_descriptor` | Unknown type or incomplete/mismatched pins accepted | `cargo test -p voom-plan compatibility_payload_requires_pinned_descriptor` |
| Sample compiled fixture matches source | Mode: focused-test; policy `policy_fixtures_test.rs::valid_policy_fixtures_match_compiled_goldens` (existing corpus case) | Fixture absent from corpus or compiled output differs | `cargo test -p voom-policy valid_policy_fixtures_match_compiled_goldens` |

1. Add the focused cases first. A red compile failure for a new public type is valid initial red; after adding shapes, keep the invalid-input assertions red until validation lands.
2. Add complete new content definitions to `compile/compiled.rs`:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledEnsureAudioCompatibilityOperation {}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledPlaceAudioCompatibilityOperation {}
```

Add exact keyword arms in both `Validator::validate_phase` (`validate.rs`) and `Validator::validate_nested_operation` (`validate/operations.rs`). Their shared validator is complete:

```rust
pub(super) fn validate_audio_compatibility_statement(&mut self, statement: &StatementAst) {
    let text = statement_text(statement);
    let tokens = words(text.as_ref());
    if !matches!(tokens.as_slice(), ["ensure" | "place", "audio", "compatibility"])
        || !matches!(statement, StatementAst::Raw { .. })
    {
        self.error(
            DiagnosticCode::UnknownPhaseStatementOrOperation,
            statement.span(),
            "compatibility operation must be exactly `ensure audio compatibility` or `place audio compatibility`",
        );
    }
}
```

`StatementAst::Raw` is the ordinary statement variant confirmed in `syntax/ast.rs`; reject the other variants and do not add new AST/parser machinery. Lower the exact three tokens in `lower_text_operation` to the two new compiled structs. Compiler validation rejects blocks before lowering; lowering also matches exact tokens and returns `unknown_operation` for anything else. Add `"ensure" | "place"` to `Parser::starts_statement_keyword_at` in `syntax/parser.rs` so same-line adjacent operations stop at these keywords. Include adjacency in `compatibility_operations_are_strict`; no AST/parser framework is added. `PolicyTool::as_str` confirms the published tool name is `mkvtoolnix`.

3. Add these complete planner descriptor and protocol settings definitions to their owning audio files; they deliberately do not introduce a planner dependency in worker crates:

```rust
// voom-plan/src/planner/audio/payload.rs
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioCompatibilityDescriptor {
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub source_channels: u32,
    pub source_channel_layout: String,
    pub target_channel_layout: String,
    pub result_snapshot_stream_id: String,
}

// voom-worker-protocol/src/operations/audio.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioCompatibilitySettings {
    pub source_snapshot_stream_id: String,
    pub source_channels: u32,
    pub source_channel_layout: String,
    pub target_channel_layout: String,
}
```

Every new optional serde field has `#[serde(default, skip_serializing_if = "Option::is_none")]`. `audio_head_stream` is singular because this operation selects one identity. Retain existing `head_streams` unchanged. Reject requests containing both nonempty global heads and an audio head; old global-only requests remain valid.

`AudioOperationPayload` currently parses manually. Extend `into_value`/`try_from_execution_value`, `AudioOperationType::as_str`, and `validate_synthesis_fields` explicitly: ensure requires positive pinned snapshot, operation ID, target channels, and strict descriptor; filter/outputs are forbidden. A planned encoding requires exactly one companion descriptor matching compatibility source/index/result ID. An ensure NoOp may carry no companions. Ordinary transcode/extract/synthesis reject `compatibility`, and ordinary synthesis retains its existing required fields. Descriptor IDs/layout strings must be nonempty, source channels positive and indexes checked. Validate each present field before classifying missing/NoOp.

4. Add the executable policy below and deterministic compiled fixture, register it in the existing fixture table, and inventory the concrete new serde units/fields. No serialized field is removed.

```text
policy "primary audio compatibility" {
  metadata {
    requires_tools: ["ffmpeg", "ffprobe", "mkvtoolnix"]
  }
  phase retain {
    keep audio where language in ["eng", "und"]
  }
  phase compatibility {
    depends_on: [retain]
    ensure audio compatibility
  }
  phase placement {
    depends_on: [compatibility]
    place audio compatibility
  }
  phase verify {
    depends_on: [placement]
    verify artifact
  }
}
```

5. Run the listed focused commands. Expect the named tests to pass with no skipped cases. Commit the vocabulary/contracts change with hooks when the caller reaches implementation; never bypass them. If abandoning this task before policies are persisted, remove its unreferenced new shapes together; after admission, follow the binary-before-policy rollback sequence in the final task.

Acceptance: exact statements compile in phases/conditionals/rules; extra tokens/blocks and unknown serde fields fail. Old compiled fixtures and old protocol JSON retain their shape. No new operation kind, worker capability, encoder or database schema is introduced. Migrate exhaustive `AudioOperationType`/`CompiledOperation` visitors and all new-field constructors in this Cast so dependent crates compile: mapping ensure to TranscodeAudio and place to Remux belongs here; behavioral resolution belongs in Casts 2 and 5. Existing unsupported-operation fallback remains until the dedicated planner branch is installed.

## Cast 2 — Deterministic resolution and executable plans

This delivers the planner's policy judgment. Ensure and place share one resolver so a second phase cannot choose a different source after a companion appears.

**Interfaces**

New module `planner/audio/compatibility.rs` is public through `planner/audio/mod.rs`. Its complete central contract is below. `resolve_audio_compatibility(snapshot: &MediaSnapshotInput) -> Result<AudioCompatibilityResolution, AudioCompatibilityError>` does not access storage, generate IDs, mutate snapshots or interpret operation retries. The caller supplies `operation_id` only when generating a new companion through existing `synthesis_companion_id(operation_id: &str, snapshot_stream_id: &str) -> String`.

**Verification**

| Contract | Mode and case in `compatibility_test.rs` unless stated | Expected red | Exact green command |
|---|---|---|---|
| Language rank, provider tie-break, commentary/derived exclusion, missing language fallback | Mode: focused-test; `compatibility_ranking_is_stable` | No resolver or choice depends on input vector/default/derived position | `cargo test -p voom-plan compatibility_ranking_is_stable` |
| Complete supported target matrix and explicit rejection | Mode: focused-test; `compatibility_target_matrix_is_exact` | Same-channel targets blocked, observed canonical alias rejected, or unsupported layouts accepted | `cargo test -p voom-plan compatibility_target_matrix_is_exact` |
| Invalid facts/identities/association precede choice | Mode: focused-test; `compatibility_rejects_invalid_facts_and_associations` | Malformed present language becomes unknown, duplicate IDs/indexes accepted, orphan/chain/duplicate companion accepted | `cargo test -p voom-plan compatibility_rejects_invalid_facts_and_associations` |
| Compatible selected original and its exact associated companion reuse; arbitrary codec peer does not | Mode: focused-test; `compatibility_reuses_only_resolved_identity` | Redundant encoding or unrelated track reused | `cargo test -p voom-plan compatibility_reuses_only_resolved_identity` |
| Ensure payload pins one original/target/derived identity and diagnostics | Mode: focused-test; `planner_test.rs::ensure_compatibility_plans_one_companion` | Unsupported operation or multiple companion descriptors | `cargo test -p voom-plan ensure_compatibility_plans_one_companion` |
| Place requires committed companion and becomes NoOp after correct placement | Mode: focused-test; `planner_test.rs::place_compatibility_requires_and_places_resolved_identity` | Place encodes, silently waits, chooses a codec peer, or rewrites an already placed file | `cargo test -p voom-plan place_compatibility_requires_and_places_resolved_identity` |

1. Write focused table-driven tests for each inventory entry. Include distinct sentinel IDs and scrambled input vector order; use provider indexes as the ordering fact. Include malformed/overflow present channels on an otherwise ineligible stream combined with no eligible source, proving corruption is diagnosed before source absence; missing irrelevant codec/layout remains permissible. Verify error codes/messages contain the offending ID or `(channels, layout)`.
2. Add the following complete target/resolution types and helpers. The resolver uses its own strict fact reader so ordinary filter behavior is untouched:

```rust
use std::collections::{BTreeMap, BTreeSet};
use serde_json::Value;
use voom_policy::MediaSnapshotInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCompatibilityTarget {
    pub codec: String,
    pub channels: u32,
    pub channel_layout: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCompatibilityResolution {
    pub source_snapshot_stream_id: String,
    pub source_provider_stream_index: u32,
    pub source_channels: u32,
    pub source_channel_layout: String,
    pub target: AudioCompatibilityTarget,
    /// None means ensure must encode; place must diagnose the missing companion.
    pub resolved_snapshot_stream_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioCompatibilityError {
    InvalidFacts(String),
    NoEligibleSource,
    UnsupportedLayout { channels: u32, layout: String },
    InvalidAssociation(String),
}

pub fn compatibility_target(channels: u32, layout: &str)
    -> Result<AudioCompatibilityTarget, AudioCompatibilityError>
{
    let codec = match (channels, layout) {
        (1, "mono") | (2, "stereo") => "aac",
        (3, "2.1" | "3.0" | "3.0(back)")
        | (4, "3.1" | "4.0" | "quad" | "quad(side)" | "4 channels (FL+FR+LFE+BC)")
        | (5, "4.1" | "5.0" | "5.0(side)")
        | (6, "5.1" | "5.1(side)") | (8, "7.1") => "eac3",
        _ => return Err(AudioCompatibilityError::UnsupportedLayout {
            channels, layout: layout.to_owned(),
        }),
    };
    Ok(AudioCompatibilityTarget {
        codec: codec.to_owned(),
        channels: if channels == 8 { 6 } else { channels },
        channel_layout: match layout {
            "quad" => "quad(side)",
            "5.0" => "5.0(side)",
            "5.1" | "7.1" => "5.1(side)",
            other => other,
        }.to_owned(),
    })
}

fn fact_error(stream: &Value, key: &str) -> AudioCompatibilityError {
    let label = stream.get("id").and_then(Value::as_str)
        .map(|id| id.chars().take(80).collect::<String>())
        .unwrap_or_else(|| "<unidentified>".to_owned());
    AudioCompatibilityError::InvalidFacts(format!("invalid {key} for stream {label}"))
}

fn text<'a>(stream: &'a Value, key: &str) -> Result<&'a str, AudioCompatibilityError> {
    stream.get(key).and_then(Value::as_str).filter(|v| !v.trim().is_empty())
        .ok_or_else(|| fact_error(stream, key))
}

fn association(stream: &Value) -> Result<Option<&str>, AudioCompatibilityError> {
    match stream.get("compatibility_source_snapshot_stream_id") {
        None => Ok(None),
        Some(Value::String(id)) if !id.trim().is_empty() => Ok(Some(id)),
        Some(_) => Err(AudioCompatibilityError::InvalidAssociation(
            "compatibility_source_snapshot_stream_id must be a nonempty string".to_owned())),
    }
}

fn language(stream: &Value) -> Result<&str, AudioCompatibilityError> {
    if stream.get("compatibility_invalid_language").is_some_and(|v| v != &Value::Bool(false)) {
        return Err(fact_error(stream, "language"));
    }
    match stream.get("language") {
        None => Ok("und"),
        Some(Value::String(value)) if value.len() == 3
            && value.bytes().all(|byte| byte.is_ascii_lowercase()) => Ok(value),
        Some(_) => Err(fact_error(stream, "language")),
    }
}

fn channels(stream: &Value) -> Result<u32, AudioCompatibilityError> {
    stream.get("channels").and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0)
        .ok_or_else(|| fact_error(stream, "channels"))
}

fn commentary(stream: &Value) -> Result<bool, AudioCompatibilityError> {
    stream.pointer("/disposition/commentary")
        .or_else(|| stream.pointer("/disposition/comment"))
        .and_then(Value::as_bool)
        .ok_or_else(|| fact_error(stream, "commentary"))
}

pub fn resolve_audio_compatibility(snapshot: &MediaSnapshotInput)
    -> Result<AudioCompatibilityResolution, AudioCompatibilityError>
{
    let streams = snapshot.stream_summary.get("streams").and_then(Value::as_array)
        .ok_or_else(|| AudioCompatibilityError::InvalidFacts("streams absent".to_owned()))?;
    let mut by_id = BTreeMap::new();
    let mut indexes = BTreeSet::new();
    let mut originals = Vec::new();
    let mut derived = Vec::new();
    for stream in streams {
        let id = text(stream, "id")?;
        let index = stream.get("index").and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| AudioCompatibilityError::InvalidFacts(format!("invalid index: {id}")))?;
        if by_id.insert(id, stream).is_some() || !indexes.insert(index) {
            return Err(AudioCompatibilityError::InvalidFacts(format!("duplicate stream identity: {id}")));
        }
        if text(stream, "kind")? != "audio" { continue; }
        // Reject malformed present facts before ranking or absence classification.
        if stream.get("channels").is_some() { channels(stream)?; }
        for key in ["codec_name", "channel_layout"] {
            if stream.get(key).is_some() { text(stream, key)?; }
        }
        if stream.pointer("/disposition/commentary").is_some()
            || stream.pointer("/disposition/comment").is_some() { commentary(stream)?; }
        if let Some(source_id) = association(stream)? {
            derived.push((source_id, stream));
            continue;
        }
        let lang = language(stream)?;
        if matches!(lang, "eng" | "und") && !commentary(stream)? {
            originals.push((if lang == "eng" { 0 } else { 1 }, index, stream));
        }
    }
    for (source_id, companion) in &derived {
        let source = by_id.get(source_id).ok_or_else(||
            AudioCompatibilityError::InvalidAssociation(format!("missing source {source_id}")))?;
        if text(source, "kind")? != "audio" || association(source)?.is_some()
            || text(companion, "id")? == *source_id
        {
            return Err(AudioCompatibilityError::InvalidAssociation(format!("non-original source {source_id}")));
        }
    }
    originals.sort_by_key(|(rank, index, _)| (*rank, *index));
    let (_, index, source) = originals.first().ok_or(AudioCompatibilityError::NoEligibleSource)?;
    let source_id = text(source, "id")?;
    let source_channels = channels(source)?;
    let source_layout = text(source, "channel_layout")?;
    let source_codec = text(source, "codec_name")?;
    let target = compatibility_target(source_channels, source_layout)?;
    let companions = derived.iter().filter(|(id, _)| *id == source_id).collect::<Vec<_>>();
    if companions.len() > 1 {
        return Err(AudioCompatibilityError::InvalidAssociation(format!("multiple companions for {source_id}")));
    }
    let associated_id = match companions.as_slice() {
        [] => None,
        [(_, companion)] => {
            if text(companion, "codec_name")? != target.codec
                || channels(companion)? != target.channels
                || text(companion, "channel_layout")? != target.channel_layout
                || language(companion)? != language(source)? || commentary(companion)?
            {
                return Err(AudioCompatibilityError::InvalidAssociation(format!("incompatible companion for {source_id}")));
            }
            Some(text(companion, "id")?.to_owned())
        }
        _ => return Err(AudioCompatibilityError::InvalidAssociation(source_id.to_owned())),
    };
    // The three encoder canonical aliases do not require another encode.
    let original_matches = source_codec == target.codec && source_channels == target.channels;
    Ok(AudioCompatibilityResolution {
        source_snapshot_stream_id: source_id.to_owned(),
        source_provider_stream_index: *index,
        source_channels,
        source_channel_layout: source_layout.to_owned(),
        target,
        resolved_snapshot_stream_id: if original_matches { Some(source_id.to_owned()) } else { associated_id },
    })
}

#[cfg(test)]
#[path = "compatibility_test.rs"]
mod tests;
```

Actual FFmpeg 8.0.1 encode→ffprobe evidence from design authoring establishes `quad`→`quad(side)`, `5.0`→`5.0(side)`, `5.1`→`5.1(side)`; `7.1` targets `5.1(side)`. These are the only admitted canonical aliases. The target helper canonicalizes output, while an already-EAC3 selected original with an admitted same-channel layout is compatible without encoding. The copied original retains its original layout. Tests must assert observed canonical output and reject any other drift.

This intentionally validates global structural IDs/indexes before choosing a stream; malformed source associations are not an invitation to encode again. Only the selected original requires missing codec/channels/layout to become present for target resolution; malformed present structural facts fail before rank/absence classification. Commentary/foreign tracks remain retained content, not fallback sources. Commentary uses the established canonical `commentary` then `comment` alias precedence with strict boolean values. The three-letter language check matches the existing private `compiled.rs::is_canonical_language_code`; do not expose that private helper merely for this use.

3. Add `plan_ensure_compatibility(phase_name: &str, snapshot: &MediaSnapshotInput, operation_id: &str) -> OperationPlan` to audio/mod.rs. It calls the resolver once. For resolved original/companion produce `NoOp`, no capability and no companion descriptors. Otherwise generate exactly one `synthesis_companion_id`, use it for both companion ID and result ID, populate the strict compatibility descriptor, and emit `Planned` with `transcode_audio`. Copy target codec/channels/layout from the resolver, never recalculate through the ordinary downmix gate.

When building human diagnostics, limit displayed IDs/layouts to 80 characters and name the offending fact; never dump complete untrusted stream JSON. Map `InvalidFacts` to `InsufficientSnapshotFacts`, `NoEligibleSource` to new `AudioCompatibilitySourceMissing`, `UnsupportedLayout` to new `UnsupportedAudioCompatibilityLayout`, and `InvalidAssociation` to new `InvalidAudioCompatibilityAssociation`. Add these three cases to `diagnostic.rs::{PlanningDiagnosticCode,as_str}` and its test. Attach target/phase/operation context through the existing diagnostic builder, using explicit ID/layout details from the error. NoEligibleSource message names English/unknown non-commentary originals.

4. In remux/mod.rs add `plan_place_compatibility(phase_name: &str, snapshot: &MediaSnapshotInput) -> OperationPlan`, returning `PlanOperationKind::Remux`. Resolve once; `resolved_snapshot_stream_id=None` is **Blocked**, with explicit ensure-not-published diagnostic, never an encoding or silent NoOp. Build a `RemuxOperationPayload` with empty track actions, empty track order (preserve source order), no global head, the resolved audio head, and one `RemuxDefaultAction { target: TrackTarget::Audio, strategy: DefaultStrategy::Preserve, selected_snapshot_stream_id: Some(resolved_id) }`. Preserve non-audio defaults through existing selection behavior. Planned iff the selected identity is not currently first audio or not sole audio default; require boolean default facts on retained audio to prove NoOp, otherwise diagnose insufficient facts. Empty track order plus audio-slot permutation preserves non-audio slots.

5. Add the explicit new operation visitor arms in `planner.rs::{expand_operation_for_snapshot,operation_kind}` and both `eligibility.rs` operation visitors. Ensure generates its operation ID through the same existing `next_operation_id` path as synthesis. Place uses its own dedicated remux plan and is not folded into an unrelated same-phase generic selector group. Existing wildcard arms in `remux::{candidate_kind,candidate_support}` stay unchanged. Keep ordinary synthesis operation visitors on their existing path.

6. Run each focused command above, then `cargo test -p voom-plan synthesize_audio_shape_blocks_when_target_is_not_a_downmix`. Expect all named tests to pass; the ordinary downmix regression must remain green. Commit with hooks. No migration or mutable state requires task-specific rollback.

Acceptance: input-vector permutation never changes selection; exact target table and the three observed canonical aliases are tested for every admitted layout; invalid/duplicate/dangling associations fail explicitly; compatible selected original is reused; unrelated compatible audio does not suppress required encoding; placement is separately planned/blocked/NoOp from refreshed facts.

## Cast 3 — Observed identities survive probe, scan and publication

This is the identity continuity seam. Existing scan and publication already preserve an `id` supplied in normalized JSON; ffprobe currently does not supply the Matroska identity tags. Do not replace these functions with a new normalization framework.

**Interfaces**

- Existing `voom_ffprobe_worker::normalize_ffprobe_json(raw: Value, provider_version: &str, probed_at: &str) -> Result<Value, WorkerError>` stays unchanged.
- Observed tag `SNAPSHOT_STREAM_ID` maps to normalized `id`; `VOOM_COMPATIBILITY_SOURCE_STREAM_ID` maps to `compatibility_source_snapshot_stream_id`, using case-insensitive keys. Both values must be nonempty strings; conflicting case variants reject as malformed worker result.
- `compatibility_invalid_language: true` means a present raw language fact was malformed rather than a harmless existing unknown sentinel. It is not a selector field and ordinary language output/filtering does not change.
- Existing `scan/publish.rs::normalize_snapshot_stream_ids(Value) -> Result<Value, VoomError>` retains fallback `stream-{index}`, and publication `normalized_snapshot(Value) -> Result<Value, VoomError>` preserves checked observed IDs.
- Existing `normalize_synthesis_snapshot(companions: &[AudioSynthesisCompanion], raw: &Value, snapshot: &mut Value) -> Result<(), VoomError>` remains the ordinary correlation boundary. Add `validate_compatibility_snapshot(settings: &AudioCompatibilitySettings, companions: &[AudioSynthesisCompanion], raw: &Value, snapshot: &Value) -> Result<(), VoomError>` before it for strict compatibility observations; no repository lookup by media tag is added.
- Extend `AdmittedAudio::normalize_snapshot(&self, raw: &Value, snapshot: &mut Value, compatibility: Option<&AudioCompatibilitySettings>) -> Result<(), VoomError>` to invoke the new validator when settings are present. `validate_outputs_in_tx` already receives `&MediaDispatch`, so it passes the exact transcode dispatch settings without changing the `AdmittedAudio` enum or ledger schema.

**Verification**

| Contract | Mode and case | Expected red | Exact green command |
|---|---|---|---|
| Strict case-insensitive observed tags | Mode: focused-test; ffprobe `normalize_test.rs::compatibility_tags_are_observed_strictly` | Tags ignored, invalid values dropped or conflicting case-variant tags admitted | `cargo test -p voom-ffprobe-worker compatibility_tags_are_observed_strictly` |
| Malformed language remains distinguishable without changing ordinary filter output | Mode: focused-test; ffprobe `normalize_test.rs::compatibility_language_marker_preserves_ordinary_semantics` | Discarded malformed language is indistinguishable from absent | `cargo test -p voom-ffprobe-worker compatibility_language_marker_preserves_ordinary_semantics` |
| Source/result identity and source association agree with admission and real probe | Mode: focused-test; publication `audio_test.rs::compatibility_probe_must_match_admitted_source_and_result` | Contradictory tag overwritten or association/source missing accepted | `cargo test -p voom-control-plane compatibility_probe_must_match_admitted_source_and_result` |
| Persisted corrupt evidence stays database error | Mode: focused-test; publication `mod_test.rs::stored_compatibility_corruption_is_database_error` | Corrupt stored evidence becomes Config/NotFound or is silently repaired | `cargo test -p voom-control-plane stored_compatibility_corruption_is_database_error` |

1. Write the named tests before implementation. Test whitespace/nonstring identity tags, case-variant conflicting tags, source/result identity mismatches, and malformed association combined with a missing source fact. Resolver tests in Cast 2 own global duplicate IDs/indexes, overflow and dangling/chain association validation. Keep error classification assertions distinct from missing-domain assertions.
2. Add this complete local tag reader to ffprobe normalize.rs; call it for the two identity tags from `insert_stream_tags`, changing that function to return `Result<(), WorkerError>` and propagating `?` from `stream_objects`:

```rust
fn observed_identity_tag(tags: &Map<String, Value>, name: &str)
    -> Result<Option<String>, WorkerError>
{
    let mut observed: Option<String> = None;
    for (key, value) in tags.iter().filter(|(key, _)| key.eq_ignore_ascii_case(name)) {
        let value = value.as_str().filter(|value| !value.trim().is_empty())
            .ok_or_else(|| malformed(format!("{key} must be a non-empty string")))?;
        if observed.as_deref().is_some_and(|previous| previous != value) {
            return Err(malformed(format!("conflicting {name} tags")));
        }
        observed = Some(value.to_owned());
    }
    Ok(observed)
}
```

Keep the existing language normalization loop unchanged. Inspect case-folded `language` entries without changing that loop: mark `compatibility_invalid_language=true` for nonstring non-null values, noncanonical non-sentinel strings, or conflicting meaningful case variants. Missing/null/empty/unknown/N/A language retains the existing unknown/untagged meaning and emits no invalid marker. A canonical value is exactly three lowercase ASCII letters. Ordinary `language` remains the existing normalized value. Reject malformed identity/association tags rather than apply this lossy sentinel filter to them.

The compatibility resolver validates present numeric/structural facts, checked provider indexes and duplicate effective IDs across the complete snapshot before selection. Existing scan/publication fallback supplies `stream-{index}` only when no observed ID exists. A tagged ID colliding with another stream's fallback ID therefore blocks compatibility resolution; it does not silently rename either stream. Do not strengthen ordinary ffprobe/scan normalization globally for this feature.

3. Preserve existing scan/publication normalization: `scan/publish.rs::normalize_snapshot_stream_ids` skips present IDs; publication `normalized_snapshot` fills absent IDs and already checks unique IDs and checked indexes. Neither needs a generic rewrite. New normalized observed tags flow through both unchanged. Fresh contradictory compatibility worker evidence uses the existing malformed-result error; decoding contradictory previously persisted evidence wraps that error through `VoomError::database` as existing `validate_owner_metadata_in_tx` does.

4. In synthesis publication, retain ordinary legacy behavior when the admitted dispatch has no compatibility settings. For compatibility mode require the observed copied original with the admitted source ID; require the actual derived ID and observed source-association tag to match the exact admitted descriptor. Require codec/channels/layout/non-default and source metadata to match the actual probe before staging/finalizing. Reject contradictory IDs rather than replacing them from result JSON. Existing ordinary synthesis normalization may still assign its expected derived ID where its legacy worker did not observe one; this is the protected compatibility path, not a fallback for new compatibility requests.

The settings discriminator is available in the immutable admitted dispatch/receipt. For persisted replay, extend `validate_original_probe` with `compatibility: Option<&AudioCompatibilitySettings>` and call `validate_compatibility_snapshot` before ordinary normalization. `validate_stored_completion_in_tx` already reads the original ticket and parses its WorkflowTicketPayload: extract/strictly deserialize only its optional `rendered_payload.media_dispatch.settings.compatibility` for this added argument before the existing original-probe check; keep the existing full dispatch decode and legacy validation order where they are. Invalid present compatibility settings become a contextual database error. Do not infer compatibility mode from codec, source channel count, tag presence or a string prefix. Keep existing claim/transaction ordering intact. Add the sibling `#[cfg(test)] #[path = "audio_test.rs"] mod tests;` declaration at the bottom of publication/audio.rs for its new test file.

5. Run the listed commands and existing `cargo test -p voom-control-plane synthesis_probe_comparison_uses_exact_admitted_companion_projection`. Expect passes, including the old ordinary synthesis projection. Commit with hooks. No schema change; do not create `0045`.

Acceptance: FFmpeg-observed tags can survive copy-remux and another scan without becoming different identities; corrupt tags fail; tags remain observations bound to authorized owner evidence, never a node identity or authorization token. Durable lineage still records the original publication and does not claim every imported tag has database ancestry.

## Cast 4 — Worker execution and audio-only placement

This delivers byte behavior independently from orchestration. The strict compatibility settings are the sole permission for same-channel companions.

**Interfaces**

- Existing `handle_transcode_audio(&TranscodeAudioRequest, &FfmpegConfig) -> Result<TranscodeAudioResult, TranscodeAudioError>` and `handle_remux(&RemuxRequest, &MkvmergeConfig) -> Result<RemuxResult, MkvtoolnixWorkerError>` remain the handler facades.
- Add `SourceAudioFact.channel_layout: Option<String>` in FFmpeg's local probe model; populate from actual `channel_layout`. Observe existing `snapshot_stream_id` tags instead of initializing observed identity to `None`.
- New local `validate_compatibility_source(request: &TranscodeAudioRequest, sources: &[SourceAudioFact]) -> Result<(), FfmpegError>` validates the independent actual-source contract before encoding.
- Existing `ordered_keep_streams<'a>(selection: &'a RemuxSelection, mapping: &MkvmergeTrackMapping) -> Result<Vec<&'a RemuxStreamRef>, MkvtoolnixError>` remains the single ordering implementation used for both arguments and actual-output validation.

**Verification**

| Contract | Mode and case | Expected red | Exact green command |
|---|---|---|---|
| Same-channel strict mode accepts only exact source/target table; ordinary mode still downmix | Mode: focused-test; FFmpeg `handler_test.rs::compatibility_request_mode_is_bounded`, `ffmpeg_test.rs::compatibility_validates_actual_source_before_encoding` | Same channels rejected universally, forged mode/settings accepted, or ordinary same-channel request encodes | `cargo test -p voom-ffmpeg-worker compatibility_request_mode_is_bounded`; `cargo test -p voom-ffmpeg-worker compatibility_validates_actual_source_before_encoding` |
| Actual output has one non-default companion, exact layout, both IDs and source association; originals survive | Mode: focused-test; `ffmpeg_test.rs::compatibility_encoding_preserves_originals_and_identity` | Missing source tag, copied source mislabeled as derived, wrong layout/default, or original content changes | `cargo test -p voom-ffmpeg-worker compatibility_encoding_preserves_originals_and_identity` |
| Audio-head permutations preserve non-audio positions and global-only ordering | Mode: focused-test; MKVToolNix `mkvmerge_test.rs::audio_head_reorders_only_audio_slots` | Selected goes ahead of video or mixed-order non-audio slots shift | `cargo test -p voom-mkvtoolnix-worker audio_head_reorders_only_audio_slots` |
| Actual remux order/default/identity mismatch rejected | Mode: focused-test; `handler_test.rs::compatibility_remux_checks_actual_order_and_defaults` | Worker trusts request IDs rather than output order/default flags | `cargo test -p voom-mkvtoolnix-worker compatibility_remux_checks_actual_order_and_defaults` |

1. Write named tests first. Use tiny generated MKVs with distinct tones for source/alternate/commentary, a subtitle and attachment; compare original encoded packet payload hashes across copy steps and actual source-derived tone. Include failure before output creation for forged source count/layout/ID and unsupported target settings.
2. Add `compatibility` request validation in handler.rs: it requires `add_track=true`, exactly one selected stream, positive target channels, nonempty source/derived distinct IDs, recognized source layout/count, and target codec/channels/layout matching the table. `compatibility=None` preserves the existing mode; add a checked actual-source guard that rejects an ordinary companion target greater than or equal to source channels (or missing source channels) before FFmpeg. Ordinary synthesized companions remain non-default.

The independent worker target validation is deliberately local; it must not depend on `voom-plan`. Its complete core after extracting the one settings/source pair is:

```rust
fn compatibility_worker_target(channels: u32, layout: &str) -> Option<(&'static str, u32, &str)> {
    let canonical = match layout {
        "quad" => "quad(side)",
        "5.0" => "5.0(side)",
        "5.1" | "7.1" => "5.1(side)",
        other => other,
    };
    match (channels, layout) {
        (1, "mono") | (2, "stereo") => Some(("aac", channels, layout)),
        (3, "2.1" | "3.0" | "3.0(back)")
        | (4, "3.1" | "4.0" | "quad" | "quad(side)" | "4 channels (FL+FR+LFE+BC)")
        | (5, "4.1" | "5.0" | "5.0(side)")
        | (6, "5.1" | "5.1(side)") => Some(("eac3", channels, canonical)),
        (8, "7.1") => Some(("eac3", 6, "5.1(side)")),
        _ => None,
    }
}
```

`validate_compatibility_source` finds the source by selected provider index, requires actual channels/layout to equal settings, and checks an actual existing ID tag against `source_snapshot_stream_id` when present (untagged originals get their pinned snapshot identity on output). Use this table to verify settings target, and check actual codec/language/commentary facts required by the selection contract. Missing source channels/layout is an explicit `OutputFactsMismatch`, never an implicit FFmpeg layout guess. This runs before spawning FFmpeg; verify no staged output appears on failure.

3. Extend only compatibility branch in `run_ffmpeg_synthesize_audio`: keep the existing map/copy/attachment/metadata behavior. Stamp copied selected original at its **original audio ordinal** with `snapshot_stream_id=<source-id>`. Stamp appended companion at `original_audio_count` with `snapshot_stream_id=<derived-id>` and `voom_compatibility_source_stream_id=<source-id>`. Do not mutate `SelectedAudioStream.source` into the result identity or stamp the copied original with the derived ID. Keep `-disposition:a:<companion> -default` and source defaults unchanged. Supply explicit `-channel_layout:a:<companion> <target-layout>` with the existing target `-ac` and encoder/bitrate. Use the resolved canonical target layout for same-channel cases; explicitly request `5.1(side)` for `7.1` downmix. Copied originals retain their source layout.

4. Populate `AudioOutputStreamFact.channel_layout` from the output probe, and verify it against compatibility target layout. Probe actual source/companion tags and ensure exactly one companion, correct channels/codec/layout, inherited language/title/commentary, false default, distinct source ID, source association and retained original count/content. Do not accept the positional fallback in `selected_output_streams` for compatibility mode: require the actual tag matches the pinned derived identity. Ordinary transcode/extract legacy positional behavior remains unchanged.

5. In MKVToolNix retain `ordered_keep_streams`'s current output assembly, including global-head behavior. Reject both head modes present. For `audio_head_stream=Some(head)`, require an exact kept reference mapped to `MkvmergeTrackKind::Audio`. After constructing ordinary `ordered`, replace only the audio slots with selected first and other audio in their existing relative order. Complete replacement core:

```rust
let is_audio = |stream: &&RemuxStreamRef| mapping
    .track_for_provider_index(stream.provider_stream_index)
    .is_some_and(|track| track.kind == MkvmergeTrackKind::Audio);
let mut audio = ordered.iter().copied().filter(is_audio).collect::<Vec<_>>();
let position = audio.iter().position(|stream| **stream == *head)
    .ok_or_else(|| MkvtoolnixError::ConfigInvalid("audio_head_stream must select kept audio".to_owned()))?;
let selected = audio.remove(position);
audio.insert(0, selected);
let mut replacements = audio.into_iter();
for stream in &mut ordered {
    if is_audio(stream) {
        *stream = replacements.next().ok_or_else(|| MkvtoolnixError::ConfigInvalid(
            "audio head permutation lost a stream".to_owned()))?;
    }
}
```

Keep the helper's return type unchanged. Existing `handler.rs::{validate_output_selection,expected_output_stream_order,validate_output_default_flags}` already recompute against this order and actual `identify_output`. Exercise those paths in the focused test; do not add a second expected-order calculation based only on returned result JSON. MKVToolNix copies stream tags; generated-media re-probe must prove they survive.

6. Run focused commands and existing `cargo test -p voom-ffmpeg-worker synthesize_audio_appends_downmixed_companion_stream`. Expect all to pass, including ordinary non-default behavior and existing global-head regressions. Commit with hooks. Failed worker output remains in existing owner-owned attempt cleanup/quarantine semantics; do not create a control-plane cleanup path.

Acceptance: the optional mode cannot turn ordinary synthesis into same-channel/default synthesis; worker verifies actual input independently; source and result identities are distinct and observed; same-channel layouts and 7.1 downmix produce the exact canonical resolved target; placement changes only audio order/defaults and retained non-audio slots remain intact.

## Cast 5 — Pinned binding, publication and retry recovery

This connects the pure resolution and actual workers to existing durable authority. No new table, ancestry lookup, publication path or retry loop.

**Interfaces**

- Extend `TranscodeAudioSelectionPlan` with `compatibility: Option<AudioCompatibilitySettings>`; ordinary returns initialize `None`.
- Existing `transcode_selection_from_payload_and_snapshot(payload: &Value, snapshot: &MediaSnapshot) -> Result<TranscodeAudioSelectionPlan, VoomError>` handles new `EnsureAudioCompatibility` before generic audio filtering.
- New private `compatibility_selection(payload: AudioOperationPayload, snapshot: &voom_policy::MediaSnapshotInput) -> Result<TranscodeAudioSelectionPlan, VoomError>` recomputes every pin using the public resolver from Cast 2.
- `remux::selection_from_payload_and_snapshot` maps `audio_head_snapshot_stream_id` to `audio_head_stream` through existing `resolved_kept_stream`, requiring `TrackTarget::Audio`.
- Existing synthesis `admit_synthesis_in_tx`, `plan_synthesis_in_tx`, `bind_synthesis_in_tx`, `finalize_in_tx` remain transaction/claim owners.

**Verification**

| Contract | Mode and case | Expected red | Exact green command |
|---|---|---|---|
| Pinned source/target/derived descriptors are recomputed; forged pins reject | Mode: focused-test; audio `selection_test.rs::compatibility_selection_revalidates_pinned_snapshot` | Mismatched source/target accepted or generic filter selects multiple tracks | `cargo test -p voom-control-plane compatibility_selection_revalidates_pinned_snapshot` |
| Wire envelope/node forwarding preserves settings exactly | Mode: focused-test; binding `binding_test.rs::compatibility_dispatch_preserves_pins`, node-agent `media_test.rs::compatibility_fields_reach_child_request` | Fields dropped/defaulted or source ID replaced by result ID | `cargo test -p voom-control-plane compatibility_dispatch_preserves_pins`; `cargo test -p voom-node-agent compatibility_fields_reach_child_request` |
| Remux audio-head must be retained audio, global-head conflict explicit | Mode: focused-test; remux `selection_test.rs::compatibility_head_binds_retained_audio` | Unknown/dropped/nonaudio head accepted | `cargo test -p voom-control-plane compatibility_head_binds_retained_audio` |
| Admission exact dispatch/claim matching includes compatibility settings | Mode: focused-test; publication `audio_test.rs::compatibility_admission_binds_exact_settings` | Forged settings sent under a valid selection/claim | `cargo test -p voom-control-plane compatibility_admission_binds_exact_settings` |
| Retry reuses canonical committed result with unchanged lineage and exact generation | Mode: focused-test; publication `audio_test.rs::compatibility_retry_reuses_committed_result` | A second operation/companion/lineage appears or stale claim succeeds | `cargo test -p voom-control-plane compatibility_retry_reuses_committed_result` |
| Corrupt stored lineage/attempt facts are decoded before conflict/absence | Mode: focused-test; store `audio_synthesis_operations_test.rs::compatibility_reuse_rejects_corrupt_canonical_lineage` | Corruption becomes NotFound/Config/re-encode instead of database error | `cargo test -p voom-store compatibility_reuse_rejects_corrupt_canonical_lineage` |

1. Write named cases first using existing DB fixtures and real time. Preserve exact mutation/event/claim ordering assertions; use synchronization/durable rows rather than sleeps to drive retry handoffs.
2. Implement `compatibility_selection`: call resolver on `planning_input`, require it still needs an encode for a dispatched ensure, reconstruct source descriptor from source ID/index and target fields, reconstruct expected derived ID via `synthesis_companion_id`, and compare payload descriptor and sole companion descriptor exactly. Snapshot ID must equal loaded `snapshot.id` before deriving worker selection. Output `TranscodeAudioSelectionPlan` has exactly one `SelectedAudioStream`: `.source` is the original fact, `.stream.snapshot_stream_id` is derived, `.stream.provider_stream_index` is original. `add_track=true`; target fields and compatibility settings come from recomputed resolution. A resolved original/companion is a planner NoOp and must not be admitted as a fresh encoding.

3. Extend precise visitors: `workflow/plan/binding.rs::render_policy_audio_payload` accepts ensure only when expected type is TranscodeAudio; `envelope.rs::audio_envelope`'s Transcode/Synthesize arm includes ensure and copies `plan.compatibility`. Keep the existing operation/capability strings. In `remux/selection.rs` bind the new ID with `resolved_kept_stream(id, Some(TrackTarget::Audio), ...)`; reject mixed heads and a head omitted from keep set. Ordinary payloads leave field absent.

4. `admit_synthesis_in_tx` currently checks only `selection.selection == dispatch.selection` before planning. For compatibility, additionally require exact target codec, target channels, add_track, profile `default` and compatibility settings agree with recomputed plan. Reject disagreement before `create_planned_in_tx` or acquiring a claim. Reuse existing `synthesize:<source-version>:<operation-id>` key and existing `NewAudioSynthesisCompanion` descriptors. Keep FileVersionId/MediaSnapshotId and other domain IDs typed; no primitive staging projection.

Publication uses actual observed source/result validation from Cast 3 and layout facts from Cast 4. `observed_synthesis_companions` includes channel_layout in the existing result facts JSON so `result_facts` retains it without schema change. Validate admitted compatibility target layout before `mark_dispatch_terminal_in_tx`, `bind_staged_in_tx`, and `record_validation_in_tx` commit their result. Preserve exact existing relative ordering of those mutations; failures roll back the caller's transaction. Existing finalization writes immutable `audio_synthesis_stream_lineage` once, then publishes the companion report/events as today.

5. Reuse `get_by_key_in_tx` + `require_committed_lineage_in_tx` for the same operation's committed-result shortcut. The latter must still require terminal exact dispatch generation and canonical lineage. A subsequent policy application resolves observed associated tags in its current snapshot; it does **not** call that function to claim imported tags have DB ancestry. Tests cover stale generation, mismatched result ID/index/layout and corrupt canonical IDs plus another missing field, asserting database classification before absence/conflict. Extend store production only if a demonstrated decoder gap is required by this contract; no new query surface is planned.

6. Run listed commands; expect all named tests pass and retry leaves exactly one ledger operation/companion/lineage for that operation key. Commit with hooks. Existing generations/quarantine/staged receipts own interrupted-attempt cleanup; never delete or rewrite canonical lineage on rollback.

Acceptance: forged plans cannot redirect source/target, exact claims remain authoritative, same operation replay does not encode again, and later applications can reuse an associated companion across remux/re-probe without new lineage or speculative DB ancestry machinery.

## Cast 6 — Generated-media acceptance and operator sample

This verifies the coupled vertical slice through authenticated owner execution. Existing CLI corpus support uses `OwnerNodeEmulator` and deterministic pseudo-media, so it cannot serve as the final-byte oracle here.

**Interfaces**

Create `crates/voom-node-agent/tests/primary_audio_compatibility.rs` with a local `CompatibilityFixture` patterned on `tests/lifecycle.rs::LiveFixture`: real TempDatabase, axum router, registered remote node, storage root, `LoadedAgentConfig`, and `AgentRuntime::new(config)?.run_until(shutdown)`. Launch real bundled ffmpeg/mkvtoolnix/ffprobe/verify workers with `WorkerConfig`; keep all byte work in the node agent. Existing `media.rs` clones selections/settings into child requests.

Use existing public control-plane methods `create_policy_document(&self, slug: &str, source: &str)`, `create_policy_input_set(&self, PolicyInputSetDraft)`, `generate_compliance_report`, and `execute_compliance_policy_with_options(&self, PolicyVersionId, PolicyInputSetId, ComplianceExecutionOptions)`. Set source/staging/output roots through fixture setup and existing execution options, not production SQL in the control plane. Existing `voom_test_support::scan_seed::seed_scanned_files(&ControlPlane, &str, StorageRootId, &[SeedFile<'_>])` can seed actual generated-byte probe evidence **before** the real owner agent starts; restore the intended root owner in test setup after this helper's temporary seeder activation, then let the real agent activate it. Read `scan_seed.rs` ownership setup before using it; do not run the seeder concurrently with the real owner.

New local media helper signatures:

```rust
pub fn generate_fixture(root: &std::path::Path, channels: u32, layout: &str,
    source_language: Option<&str>, source_codec: &str) -> std::io::Result<std::path::PathBuf>;
pub fn probe(path: &std::path::Path) -> std::io::Result<serde_json::Value>;
pub fn packet_hashes(path: &std::path::Path) -> std::io::Result<std::collections::BTreeMap<String, Vec<String>>>;
pub fn assert_selected_tone(path: &std::path::Path, provider_index: u32,
    expected_hz: f64, rejected_hz: &[f64]) -> std::io::Result<()>;
```

Implement only this test's narrow subprocess helpers. Existing `voom-cli/tests/support/media_inspect.rs::{ffprobe,mkvmerge_identify,assert_stream_tone}` and `support/process.rs::run_bounded` are grounded reference implementations, not importable library APIs. Copy their bounded subprocess/decoded-tone approach into the new support file, with names above; do not move shared infrastructure across crates or build a framework. Missing external tools fail this required test with diagnostics, never silently skip. Existing CI installs FFmpeg and MKVToolNix on Linux/macOS.

**Verification**

| Contract | Mode and concrete test | Expected red | Exact green command |
|---|---|---|---|
| Actual rank/retention/default/order/content with real owner workflow | Mode: focused-test; `primary_audio_compatibility.rs::primary_audio_compatibility_executes_on_owner` | New DSL blocked, wrong source tone, foreign audio retained, commentary/original/subtitle/attachment lost, wrong actual audio/default order | `cargo test -p voom-node-agent --test primary_audio_compatibility primary_audio_compatibility_executes_on_owner -- --nocapture` |
| Complete admitted channel/layout target matrix actual encoding and compatible-original no encode | Mode: focused-test; same file `compatibility_generated_target_matrix` | Actual codec/channels/layout differs, original encoded payload changes or a compatible original still gets companion | `cargo test -p voom-node-agent --test primary_audio_compatibility compatibility_generated_target_matrix -- --nocapture` |
| Interruption after ensure publication, re-probe and later placement/reapply do not encode again | Mode: focused-test; same file `compatibility_recovers_between_ensure_and_place` | Companion count or synthesis lineage grows, selected source changes after reordering, companion loses association | `cargo test -p voom-node-agent --test primary_audio_compatibility compatibility_recovers_between_ensure_and_place -- --nocapture` |
| Runbook prose and deployment guidance | Mode: task-test-not-applicable | No executable consumer validates the prose; meaningful behavioral and sample syntax coverage is assigned above, so a wording snapshot would only lock editorial text | Read source policy and runbook against the observed passing fixture; do not invent a prose test |

1. Implement the fixture with small generated MKVs (about two seconds, low-resolution copied video), tones that distinguish every original, subtitle text and an attachment with known bytes. Use `anullsrc=channel_layout=<layout>:sample_rate=48000` plus a tone in a real channel, avoiding `-ac`-only generation that guesses the layout. Assemble interleaved audio/video/subtitle order with mkvmerge so an incorrect global-head implementation is observable. Set an English commentary track earlier than eligible English and an unknown original earlier than eligible English; ensure neither wins. Include a foreign-language original to prove removal. Assert fixture preconditions using ffprobe before execution.

2. Run the shipped sample through real policy creation, input snapshot persistence and compliance execution while the real authenticated owner agent and bundled children run. Inspect the **committed final locator** from durable result rows, not an intermediate staging file or reported order. Re-probe with ffprobe/MKVToolNix: compatibility audio must be first audio by actual index and sole audio default; non-audio sequence/slots must match the input to placement; non-audio defaults/forced flags, commentary, titles/languages and attachment bytes survive. Verify selected companion tone matches ranked source and not alternate/commentary/foreign tones. Compare original encoded packet data hashes keyed by original identity (before initial stamping use known track title/tone) across ensure/place; lossy companion itself uses tone/channel/layout evidence, not decoded equality. Check no copied original is mislabeled as the derived stream.

`packet_hashes` runs ffprobe `-show_packets -show_data_hash sha256 -show_streams -of json`, groups packet `data_hash` values by stream identity, and compares retained original sequences. Attachment extraction uses mkvextract with its actual attachment ID and compares bytes. Both operations run in test code, never the production control plane.

3. `compatibility_generated_target_matrix` covers every supported pair in the design table using the real handler/runtime path, including mono, stereo, each admitted 3/4/5/6-channel layout, and 7.1 → EAC3 5.1(side). Use FLAC/PCM originals where needed to force encoding; retain originals and verify target layout independently. Include AAC mono/stereo and EAC3 5.1 selected originals: ensure is NoOp, companion/ledger count stays zero, placement still makes them first/default when needed. Include unknown/untagged fallback with no eligible English source. Unsupported layout/count and absent eligible source cases belong in focused resolver tests and at least one real workflow blocked-before-mutation fixture. Do not infer actual encoder support solely from `-h encoder=eac3`.

4. Model interruption without a sleep race: execute a policy ending at ensure; wait for its committed operation and canonical lineage; stop the owner cleanly; re-probe/reseed that actual result as a later application input; restart an owner and execute the full policy. Ensure must be NoOp and placement must reuse exactly that derived ID. Execute full policy once more on final committed input: ensure and place NoOp, one derived track total, stable identity and unchanged synthesis ledger count. Separately the publication focused test in Cast 5 retries the same exact dispatch/claim operation; these are distinct recovery contracts.

5. Update `docs/runbooks/operator-real-media-execution.md` with the sample path, required phase dependencies, layout bounds, explicit failure messages, and binary-before-policy rollout. No output-shape claim substitutes for byte assertions. No library report or real-library execution is included.

6. Run each focused acceptance command and then `just ci`. Expected result: all commands exit 0; the new named tests run and pass; zero silently skipped acceptance cases. Record any existing suite skips explicitly. Baseline failures discovered before code changes remain separate evidence; a new failure must be resolved before push. Review any intentional insta changes interactively via `cargo insta review`; do not bulk accept unrelated snapshots. After meaningful concurrency/recovery changes, use `just test-repeat voom-node-agent compatibility_recovers_between_ensure_and_place 5` to prove repeated convergence, then stop repeating once it passes without new concerns.

Acceptance: C1–C5 have actual final-byte evidence, repeat/interruption identity stays stable, and the sample is executable through the supported owner workflow. Shutdown each fixture, terminate/reap children, and remove only its TempDir; never touch user library media. Rollout installs compatible policy/planner/control-plane/agent/probe/transform binaries before admitting new policy variants. Rollback first stops new-policy admission and drains/quarantines affected work through existing authority; it does not downgrade old readers across new durable variants or delete lineage. Optional migration reservation 0045 remains consumed and unused.

## Coverage and completion checkpoint

| Frozen criterion | Implementation | Required proof |
|---|---|---|
| C1 language retention/rank/fallback/commentary | Casts 1,2,3,6 | Strict compile, rank permutations, malformed-language distinction, real retained content/source tone |
| C2 originals/targets/no redundant encoding | Casts 2,4,5,6 | Full target matrix, exact pinned selection, real target layouts and original packet hashes, compatible-original NoOp |
| C3 first audio/sole default/preservation | Casts 2,4,5,6 | Audio-slot permutation + real remux output validation + committed final probe/content |
| C4 explicit diagnostics/retry/reapply/interruption | Casts 2,3,5,6 | Bad facts/association tests, exact claim/lineage replay, real ensure-stop-place/reapply |
| C5 executable sample/generated media | Casts 1,6 | Corpus fixture and real authenticated owner acceptance; no pseudo-media oracle |

No plan deferrals or adjacent follow-ups are introduced. Exclusions remain owned exactly as frozen: encoder expansion/readiness #758/#759–#764; real-library work #339; ordinary synthesis #737/ADR 0110; general-framework reconsideration campaign operator; byte ownership ADR 0050/separate approved architecture work. The complete design set receives one bounded independent review and one scope audit before implementation.
