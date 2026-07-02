---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0030 — Issue action CLI: operator read + transition surface

## Context

Issues are opened by the compliance path (`policy_noncompliant`, ADR 0018's
`terminal_failure`) but were **write-only**: no command read, listed, or
transitioned them, and the schema's `severity` / `priority` / `priority_source` /
`priority_reason` / `suppressed_until` fields (spec §10.2) had no operator
surface. T5 (#274) generalized `SqliteIssueRepo` to back both issue kinds, so a
single read/transition surface can serve every kind.

Two contract questions had to be settled: which durable events an operator
transition emits, and how the list command paginates.

## Decision

**`voom issue list|show|update|resolve|suppress|accept` over the generalized
issues repo, with keyset pagination and the existing three-verb event taxonomy.**

### Generalized read surface

`SqliteIssueRepo` gains a `pool` (it was a ZST holding only `_in_tx` writers) and
projects the full row as `IssueRecord`, exposing the complete `issues.status`
vocabulary (`IssueStatus`: open/planned/resolved/suppressed/accepted) — a
superset of the compliance write path's `PolicyIssueStatus`. `list_issues` filters
by status/kind/priority/severity and paginates; `get_issue` reads one by id.

### Transitions preserve the schema's timestamp invariants

The `issues` table's CHECK constraints tie `status = 'resolved'` to a non-null
`resolved_at` and `status = 'suppressed'` to a non-null `suppressed_until`. Each
transition sets the target status and explicitly nulls the timestamp columns that
no longer apply (resolve nulls `suppressed_until`; suppress nulls `resolved_at`;
accept nulls both), so no transition can violate a CHECK. `update` overrides
priority only and stamps `priority_source = 'user'` (the spec's operator-override
path), leaving status — and therefore the timestamp invariants — untouched.

### Transition events reuse the three-verb taxonomy

The durable event vocabulary has exactly three issue verbs
(`issue.opened` / `issue.updated` / `issue.resolved`). Operator transitions map
onto it rather than growing it: `resolve` → `issue.resolved`; `update`,
`suppress`, and `accept` all → `issue.updated`. The `IssueLifecyclePayload.status`
field carries the specific new state (`suppressed` / `accepted` / …), so the
distinction is durable without new event kinds. Each transition composes the repo
`_in_tx` write with one event in a single transaction (the one-transition-one-
event rule); a transition against an unknown id writes nothing and returns
`Ok(None)` → CLI `NOT_FOUND`.

### Keyset pagination, matching `EventRepo::list`

`list` orders by ascending id and returns `next_cursor` = the id of the last row
returned (`None` only for an empty page), resumable via `--after-id`. This is the
same forward-list convention `EventRepo::list` already uses and the shared
convention T17 (#286) adopts for the inspection commands. `suppress` takes
`--days` (a relative horizon computed from the injected clock) rather than an
absolute timestamp, keeping domain time under the `Clock` and out of CLI arg
parsing.

## Consequences

- Operators can triage every issue kind: list/filter, inspect, override priority,
  and resolve/suppress/accept, each emitting an auditable event.
- No migration and no new event kinds: the surface rides the existing schema and
  taxonomy.
- Transitions are permissive (any id, any current status). Re-resolving an
  already-resolved issue re-stamps `resolved_at` and bumps `epoch`; this models a
  deliberate operator action rather than the compliance path's live-only guard. A
  status-transition state machine is deferred until a consumer needs one.

## Considered & rejected

- **New `issue.suppressed` / `issue.accepted` event kinds.** Rejected: the event
  vocabulary is public contract, and the payload's `status` field already records
  the new state; two more kinds add surface for no reader that distinguishes them.
- **Absolute `--until <rfc3339>` for suppress.** Rejected: it would pull timestamp
  parsing and timezone handling into the CLI and bypass the injected `Clock`;
  `--days` keeps domain time in one place.
- **A separate offset/limit pagination.** Rejected: keyset on id is stable under
  concurrent inserts and matches the established `EventRepo` convention (#286).
