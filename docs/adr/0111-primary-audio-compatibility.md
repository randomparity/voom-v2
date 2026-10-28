# ADR 0111: Primary audio compatibility is ensured before placement

## Status

Accepted (2026-10-28)

## Context

Issue #765 requires one ranked primary compatibility track while preserving
retained originals. ADR 0026 only permits strictly downmixed companions and
ADR 0110 requires ordinary synthesized companions to remain non-default.
The operator approved a bounded two-stage extension on 2026-10-28.

## Decision

Add `ensure audio compatibility` and `place audio compatibility`. The former
selects the first English non-commentary original in provider order, falling
back to unknown/untagged originals. It resolves AAC for mono/stereo, EAC3 for
supported 3–6-channel layouts, and EAC3 5.1 for 7.1. A compatible original or
its already-published compatible companion is reused. A new companion remains
non-default and uses the existing owner-node synthesis publication ledger.

Placement is a later copy-remux over refreshed committed facts. It puts the
resolved compatibility identity first within the audio group and makes it the
sole audio default. An additive audio-group head contract preserves the
existing global-head remux behavior and non-audio ordering.

Carry source, companion and retained stream identities through pinned payloads,
namespaced observed media tags, snapshots and existing durable synthesis lineage. Tags are
untrusted facts: validate structure, unique identities, association and target
facts before reuse. Do not identify companions merely by codec or language.
Existing operation claims, generations, publication transactions and owner-node
byte access remain authoritative. No new encoder or execution framework.

This extends the strictly-downmix restriction only for the new compatibility
operation. Ordinary synthesis retains ADR 0026 and ADR 0110 semantics.

## Consequences

Policies explicitly sequence retention, ensure, placement and verification.
An interruption after companion publication leaves a reusable non-default
companion; another application completes placement without another encoding.
New optional wire fields require updated workers before policies using them
are admitted; older policies retain their existing serialized forms.
The existing lineage retains the association without a schema migration;
migration reservation 0045 remains consumed and unused.

## Considered & rejected

- Combine synthesis and default changes in one operation. judgment: obscures
  the approved post-publication placement boundary and ordinary synthesis contract.
- Add generic ranked selectors and target decision tables. judgment: expands
  a bounded compatibility workflow into an unrelated media-operation framework.
- Use the existing global head for placement. verified: at base 20cdc73d,
  `sed -n '52,60p' crates/voom-worker-protocol/src/operations/remux.rs`
  describes head streams ahead of group order, including video; that is an
  unnecessary change to non-audio ordering for this requirement.
- Do nothing. judgment: cannot meet #765's same-channel companions and
  deterministic primary-source placement requirements.
