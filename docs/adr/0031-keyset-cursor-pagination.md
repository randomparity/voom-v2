---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0031 — Keyset cursor pagination for durable-row inspection commands

## Context

The `voom` CLI is the agent-facing inspection surface for the control plane's
durable state. Before this change two problems compounded:

- **Missing inspection surfaces.** Several durable row families the daemon
  consumes had no CLI reader at all: `events`, `jobs`, `tickets`, and the
  scheduler `leases` table. The Foundation milestone requires every durable
  family to be inspectable with stable ids.
- **Non-deterministic, unstable paging.** The list commands that did exist
  (`artifact list`, `scheduler decisions list`, `backup list`, `bundle list`)
  took a bare `--limit` row cap and nothing else. An agent could ask for the
  first 100 rows but had no way to ask for "the next 100": there was no cursor.
  Worse, offset-style paging over a table that is being appended to
  concurrently is not stable — a row inserted between two page fetches shifts
  every subsequent offset, so an agent walking a large library either
  re-processes or skips rows.

## Decision

**Adopt a single keyset (a.k.a. seek) cursor convention for every list command
that pages over a durable, append-heavy, integer-`id`-keyed row family, and add
the four missing inspection surfaces on top of it.**

### The convention

1. **Stable order is by primary `id`, descending (newest first).** Every
   covered list orders strictly by the table's autoincrement `id`. `id` is
   unique and monotonic with insert order, so the order is total and
   deterministic — no ties, no dependence on a mutable column.

2. **`--after-id <id>` is an exclusive continuation token.** It returns rows
   that come *after* the given id in the command's order — i.e. `id < after_id`
   for the descending order above. Omitted, paging starts at the newest row.
   Keying the continuation off the immutable primary id (not a numeric offset)
   is what makes paging stable under concurrent appends: a newly inserted row
   gets a larger id and only ever appears at the head of the sequence, never
   inside a window the caller has already walked.

3. **`next_cursor` is a top-level envelope field.** It carries the `id` of the
   last row in the page, and is present **only when the page was full**
   (`returned == limit`) — i.e. when more rows may exist. When the page is short
   the field is omitted, which is the unambiguous end-of-stream signal. An agent
   pages by feeding `next_cursor` back as `--after-id` until the field is
   absent. `next_cursor` sits beside `status`/`data`/`warnings` in the envelope
   and is `skip_serializing_if = "Option::is_none"`, so non-list commands and
   exhausted pages are byte-identical to before.

4. **Filters compose with the cursor.** Entity/kind/time/state/etc. filters
   narrow the set; the cursor advances within the filtered set. For commands
   whose "state" is derived in-app rather than stored in a column (`artifact
   list`), the cursor keys off the last *scanned* id, so a page that filters out
   every scanned row still advances and never loops.

### Scope of the convention

Applied to: `event list`, `job list`, `ticket list`, `scheduler leases list`
(the four new surfaces), and the existing `artifact list`, `scheduler decisions
list`, `backup list`, `bundle list`.

**Deliberately excluded:** the registry/config list commands — `node list`,
`worker list`, `library list`, `library root list`, `policy list`, `profile
list`, `scheduling-policy list`, `safety-policy list`. These enumerate small,
operator-managed sets (registrations, policy documents, seeded profiles), not
append-heavy per-library row families, and are returned whole. Bolting a cursor
onto them would add ceremony without solving a real paging problem. If one of
these ever grows unbounded it adopts the same convention — the helper is shared.

### New inspection surfaces

- **`voom event list`** — filter by `--kind`, `--subject-type`, `--subject-id`,
  and a `--since` / `--until` occurred-at window; plus `voom event show
  --event-id`.
- **`voom job list` / `voom job show`** — filter by `--state`.
- **`voom ticket list` / `voom ticket show`** — filter by `--state`.
- **`voom scheduler leases list` / `voom scheduler leases show`** — the
  scheduler-owned `leases` table, filter by `--state`. Nested under the existing
  `voom scheduler` group rather than a top-level `voom lease` so it does not
  collide with the manual use-lease `voom lease` command (T13/#282); the two are
  distinct durable families (scheduler execution leases vs. operator use-leases).

## Consequences

- `scheduler decisions list` and `backup list` previously ordered by
  `created_at` (with `id` as a tiebreak); they now order by `id` only. For
  append-only tables where `created_at` is assigned at insert this is the same
  visible order, but it is now guaranteed total and cursorable.
- An agent can walk an arbitrarily large family to completion with a fixed
  memory footprint and exactly-once row delivery, even while the daemon appends.
- The envelope gains one optional field; the CLI output contract is otherwise
  unchanged.
