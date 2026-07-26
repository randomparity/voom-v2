# Probe Container Canonicalization Design

**Issue:** #354
**Status:** Draft
**Base:** `main` at `de51a0da8d02d17ff5dc817e405af47516fd874f`

## Goal

Project supported ffprobe container names into the canonical policy vocabulary
before planning so stored reports and coordinator phase planning agree with the
media bytes they inspect. A successfully produced MKV artifact must replan as
compliant when its policy is already satisfied.

## Context

Durable `MediaSnapshot.payload` stores normalized probe evidence without
rewriting it. ffprobe describes Matroska with values such as
`matroska,webm`, while compiled policies and planner payloads use `mkv`.
`media_snapshot::planning_input` currently copies the probe value verbatim.
Stored planning and coordinator planning both refresh their facts through that
projection, so an MKV output can immediately plan the same mutation again.

The raw durable payload remains useful inspection evidence and is already part
of stored history. This change does not alter the probe worker, persisted JSON,
or policy grammar.

## Invariants

- Durable probe payloads remain byte-for-byte unchanged.
- Every durable `MediaSnapshot` used for planning crosses one canonicalization
  boundary.
- The boundary returns only canonical policy container names or no fact.
- Unknown or malformed values never become a guessed container.
- Caller-supplied offline `MediaSnapshotInput` values are already policy-domain
  inputs and are not reinterpreted as probe output.
- No policy syntax, compiled-policy wire shape, database schema, or worker
  protocol changes.

## Decision

### Boundary

`crates/voom-control-plane/src/media_snapshot.rs` owns a private pure
canonicalizer used by `planning_input`. The projection continues to accept the
two durable payload shapes already supported:

```json
{"container": "mkv"}
{"container": {"format_name": "matroska,webm"}}
```

The selected string is passed through the canonicalizer before becoming
`MediaSnapshotInput.container`.

This existing projection is already used by:

- authoritative stored plan and compliance-report generation;
- whole-library and scan-derived stored input refresh;
- coordinator per-phase planning and regenerated phase reports;
- remux and audio runtime selection; and
- transcode copy decisions.

No caller receives a second mapping implementation.

### Exact mapping

The accepted values are exact, case-sensitive strings:

| Durable value | Policy value | Evidence |
|---|---|---|
| `mkv` | `mkv` | canonical legacy/test payload |
| `matroska` | `mkv` | ffprobe/fake-worker Matroska value |
| `matroska,webm` | `mkv` | normalized real MKV probe value |
| `mp4` | `mp4` | canonical legacy/test payload |
| `mov,mp4` | `mp4` | existing ffprobe worker fixtures |
| `mov,mp4,m4a,3gp,3g2,mj2` | `mp4` | real MP4 ffprobe fixture |
| `ogg` | `ogg` | published audio-extract container |

The implementation uses an exact `match`, not token-set inference. It does not
trim, lowercase, reorder, deduplicate, or accept a subset/superset of a listed
value.

The projection accepts either the legacy top-level container string or the
normalizer's container object when that object has a string `format_name`.
Other fields in the normalizer object remain raw inspection evidence and do
not affect planning. Missing container values, `null`, arrays, numbers,
booleans, objects without `format_name`, and objects whose `format_name` is not
a string project to no fact.

Unrecognized strings that project to no fact include:

- empty or whitespace-padded strings;
- uppercase or mixed-case values;
- `webm`, `mov`, and `m4a` alone;
- reordered or duplicated aliases;
- known aliases mixed with an unknown token; and
- `format_long_name` without `format_name`.

The strict table prevents a future ffprobe spelling from silently becoming a
different policy container. Supporting another spelling requires an explicit
code-and-test change.

### Failure behavior

An unrecognized or malformed container projects as
`MediaSnapshotInput.container = None`. Existing planner behavior then emits a
blocked node with the actionable insufficient-facts diagnostic rather than
planning from an invented value.

The projection does not return `Result`: unknown probe vocabulary is an absent
planning fact, not corrupt persisted data. Other usable snapshot facts and raw
inspection evidence remain available.

### Stored reports and coordinator planning

`resolve_stored_planning_input` continues replacing cached input-set facts with
the active durable snapshot, but now receives a canonical container. Both
`plan_accepted_policy_version_with_input_set` and
`generate_compliance_report` therefore report the same canonical observation.

Coordinator dispatch planning and post-commit report regeneration already call
the same projection for each chain-tip snapshot. Repeating an already-satisfied
MKV transform becomes `NoOp`; no second ticket or artifact is created.

An existing phase-barrier lineage test currently relies on the alias bug to
force a second identical transcode. Its policy becomes:

1. phase `normalize`: `transcode video to hevc`;
2. phase `archive`, depending on `normalize`:
   `transcode video to hevc using profile "hevc-archive"`.

The first phase produces default HEVC with `yuv420p`; the second remains
independently necessary because `hevc-archive` requires `main10` and
`yuv420p10le`. The test continues pinning two committed versions, the distinct
`default-hevc` then `hevc-archive` target paths, phase-specific pixel-format
observations, the V0 -> V1 -> V2 `produced_from` chain, and both reprobe
snapshot identities. Coordinator alias behavior gets a separate focused
planning test.

### Generated-media evidence

The existing real-media flows retain assertions that committed snapshots store
the raw `matroska,webm` value. Their authoritative stored replanning assertions
change from `Planned` with a raw observed container to `NoOp` with canonical
`mkv`.

Coverage includes:

- remux output;
- video transcode output;
- named-profile video transcode output; and
- audio transcode output.

The remux flow gains the authoritative replan assertion it does not currently
have.

## Compatibility and rollout

There is no migration. Existing raw snapshots are canonicalized when read for
planning, so old and new rows behave identically. Persisted inspection,
lineage, and event payloads remain unchanged.

Rollback consists of reverting the projection and tests. It would restore
redundant planning but would not require data repair.

## Security and operational impact

No authentication, authorization, path, process, or network boundary changes.
The fail-closed mapping reduces the chance that malformed external probe facts
authorize an incorrect mutation plan.

No new logs or metrics are needed. Plans and reports already expose a blocked
diagnostic for missing container facts and canonical observed state for known
facts.

## Test strategy

### Unit and stored-path tests

- Table-test every accepted mapping.
- Exercise both accepted extraction shapes: a legacy container string and a
  normalizer object with string `format_name` plus unrelated inspection fields.
- Reject missing, wrong-typed, object-without-string-`format_name`, empty,
  padded, case-shifted, reordered, duplicated, subset, superset, and
  mixed-unknown values.
- Assert stored plan and report paths reproject `matroska,webm` as `mkv` and
  produce `NoOp` for `container mkv`.
- Assert unknown and malformed durable values produce a blocked
  insufficient-facts node in both stored plan and report paths.
- Assert coordinator phase report generation receives the same canonical
  value and produces `NoOp`.

### Generated-media tests

- Keep raw committed-snapshot assertions at `matroska,webm`.
- Assert remux, video transcode, named-profile transcode, and audio transcode
  outputs replan as `NoOp`.
- Assert the `NoOp` observed state carries `mkv`, not the raw ffprobe alias.

### Guardrails

- Focused `voom-control-plane` unit and integration tests.
- `prek run` before each implementation commit.
- `just ci` before shipping and again after any rebase.

## Documentation

Update ADRs 0007, 0008, and 0009 to remove the raw-alias limitation and state
that durable snapshots are canonicalized at the planning projection boundary.
Historical decision text remains factual about the behavior at the time; the
current consequences and resume rationale must no longer claim replanning is
unsafe because of container aliases.

## Out of scope

- New policy container names or DSL forms.
- Changing ffprobe normalization or durable snapshot JSON.
- Inferring container from file extensions, MIME types, long names, codecs, or
  stream layouts.
- Audio execution and lineage work in #99, #333, and #337.
- Attachment/commentary/default/order execution in #331, #332, and #336.
