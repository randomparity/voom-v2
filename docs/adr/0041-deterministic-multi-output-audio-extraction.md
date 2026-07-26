---
status: accepted
date: 2026-07-26
deciders: [VOOM core]
---

# 0041 — Deterministic multi-output audio extraction descriptors

## Context

The published `extract audio [where <track-filter>]` production permits an
omitted or broad selector, but the planner and worker contract require exactly
one matching stream. Valid policies therefore block on ordinary media with
multiple audio streams.

Extraction is a sidecar-producing operation. Expanding it safely requires more
than changing the cardinality check: every output needs a stable identity,
source-stream lineage key, canonical order, and collision-safe name before
workers execute. Retry and resume must address the same outputs. The existing
host commits one sidecar per ticket, while issue #337 owns the atomic plural
host commit and reporting unit.

Historical single-output plan/worker data and compiled policies must remain
readable. ADR 0013 therefore requires additive fields, while ADR 0016 allows
the bundled worker behavior to move in lockstep.

Design:
[`docs/superpowers/specs/2026-07-26-issue-99-deterministic-audio-extraction-design.md`](../superpowers/specs/2026-07-26-issue-99-deterministic-audio-extraction-design.md).

## Decision

### Resolve ordered descriptors during planning

The planner resolves all matching source audio streams, requires unique
provider stream indexes, sorts by ascending provider stream index, rejects zero
matches, and validates the bundle role of every match. It emits one ordered
descriptor containing output ID, source snapshot stream ID/index, final
name suffix, and bundle role.

The operation ID is the existing deterministic plan node ID. Output IDs use
the exact domain-separated BLAKE3 preimage and public encoding specified by the
design. Identity is fixed before ticket creation and reused by retry/resume.

### Detect final-name collisions

The one-output suffix remains
`<sanitized-stream-id>.opus.ogg`. For a plural operation, every descriptor
receives its fixed-width output hash suffix after the exact sanitization and
ASCII case-folding consumed by target paths. Fixed-width trailing hashes make
the rewrite injective unless the truncated output hash itself collides. A final
global normalized-name uniqueness assertion fails closed in that case. No
later layer re-sanitizes descriptor names.

### Evolve the worker contract additively

Extract requests and results gain presence-preserving optional ordered output
descriptor lists. Existing singular fields remain the required first-output
projection. An absent list means legacy singleton data; an explicitly empty
list and explicit `null` are invalid; a present non-empty list carries every
output including the first. A presence-aware serde codec omits the legacy state
on serialization and rejects present non-array values. Validation rejects any
disagreement, duplicate, omission, addition, or reorder.

The FFmpeg worker validates the complete descriptor set before provider work
and returns success only with one ordered result per request descriptor. A
partial provider failure produces no success result.

### Keep the host commit boundary explicit

Before #337, the control plane accepts historical or new singleton extraction
payloads and rejects plural payloads before preparing staging/target paths or
dispatching a worker. #337 replaces that guard with all-output staging,
validation, atomic commit, bundle membership, durable lineage, recovery, and
reporting.

No grammar or compiled-policy change is made.

## Consequences

- Bare and broad selectors produce stable ordered planner outputs instead of a
  multiple-match block.
- Zero matches and malformed/insufficient source identity still fail visibly
  before execution.
- One-match policy behavior, target naming, and legacy singular JSON remain
  readable and unchanged. Plural names always carry output hashes.
- Worker execution can produce every planned sidecar under one operation while
  the host cannot accidentally commit a subset before #337.
- The worker wire shape contains a redundant first-output projection. The
  redundancy is bounded by strict validation and is the cost of the required
  additive compatibility.
- Rollback across newly written additive fields follows ADR 0013: an older
  binary rejects unknown fields loudly, so operators restore the matching
  pre-upgrade database when downgrading.

## Considered and rejected alternatives

### Add new DSL syntax for a plural extraction mode

Rejected. The published optional selector already means all matching streams.
Another form would be parser-only product divergence and would leave bare
extraction broken.

### Reevaluate the selector independently during execution

Rejected. Re-evaluation can preserve membership but cannot prove output
identity, collision decisions, or ordering stayed fixed across retry/resume.
Execution validates planned descriptors against the pinned snapshot instead.

### Use provider indexes or titles in output identity/names

Rejected. Provider indexes are worker selectors and titles are mutable,
optional metadata. Durable snapshot stream IDs define lineage. Provider indexes
only define canonical order.

### Replace the singular wire shape with an incompatible plural shape

Rejected. It is cleaner but makes historical single-output worker data
unreadable and violates the required additive payload evolution. A custom
legacy parser was also rejected because it creates two hidden wire schemas;
the explicit invariant-checked first projection keeps compatibility visible in
the type.

### Commit each worker output independently

Rejected. A short or malformed result could leave a partially committed bundle.
Issue #337 owns one all-or-none host transaction and recovery unit.
