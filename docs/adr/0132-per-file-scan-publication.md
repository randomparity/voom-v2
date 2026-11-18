# ADR 0132: Publish agreed scan evidence at batch acceptance

## Status

Proposed

Drafted 2026-11-17 for #841. If accepted it supersedes ADR 0077's completion-only publication
and amends two parts of ADR 0067: the consequence that batch acceptance is provisional catalog
evidence, and the decision that failed, cancelled and stale sessions never update file
locations (they still never retire one). ADR 0077
stays Accepted, and governs, until the maintainer decides; implementation needs its own issues.

## Context

ADR 0077 publishes scan identity only inside the completion transaction, so nothing a scan
learns is durable for policy until the whole root has been read. ADR 0120 rejected per-file
readiness because first durable observation had never been measured and publishing earlier
would supersede ADR 0077.

#841 measured it with an unmerged prototype (`spike/per-file-publication-841` at `2638c04a`)
on one release build: full local corpus (34 files, 935.3 GB, btrfs), evicted, probe hash reuse
off, three repetitions per cell, G3's stage-0 cell as control
([records](../media-evidence-baseline/per-file-publication/README.md)):

| Publication | First durable observation, median | Complete, median | Batches |
|---|---:|---:|---:|
| completion (ADR 0077) | 678.1 s | 678.1 s | 2 |
| per batch, one observation per batch | 10.7 s | 669.8 s | 34 |

First durable observation improves by 98.4% under the protocol's thresholds; complete time,
reads and CPU show no regression. The same content hashes were published in every repetition.

ADR 0077 rejected "publish identity during batch acceptance" because a failed session would
leave partial catalog mutations that cleanup must undo. ADR 0067 rejected stamping
`last_scan_session_id` onto locations as batches arrive, because a failed or malicious partial
stream would mutate authoritative catalog state and make later absence classification depend
on cleanup of provisional writes. Both grounds concern absence. Absence is decided only at
completion, from the session's observed locators against its start high-water mark; a location
the session creates sits above that mark, so it is never one of that session's retirement
candidates, and nothing here stamps `last_scan_session_id` early. Published identity is not
absence state, and the owner agent is already trusted to submit the evidence completion
publishes verbatim: per-batch publication changes when that evidence becomes durable, not who
may supply it.

## Decision

Publish each batch's agreed evidence when the batch is accepted, and keep absence
reconciliation at completion.

- **Publication.** Inside the batch-acceptance transaction, after the batch's observation rows
  are accepted, publish each evidence-bearing observation of the batch with ADR 0077's existing
  logic (same-address replay, hardlink attach, fresh ingest, media snapshot, sidecar bundle)
  under a savepoint, and mark the batch published. A publication error rolls back the whole
  batch. Exact replays and ledger replays return the stored outcome and publish nothing.
- **Completion publishes only unmarked batches.** Completion publishes the evidence of any
  accepted batch not marked published, in sequence order, before reconciling absence. That
  covers a batch accepted by a control plane that predates this decision, for example one
  upgraded while a scan runs; in steady state it publishes nothing. The mark is an additive
  column (ADR 0013).
- **Cadence.** The owner pump submits each observation without waiting to fill a batch. The
  1,000-observation and byte budgets stay as upper bounds.
- **Absence reconciliation is unchanged.** Completion stays the only absence linearization
  point. Its retirement candidates stay pre-start live locations the session did not observe,
  and `library_roots.last_scan_session_id` still moves only on success.
- **Ticket and lease authority is unchanged.** Publication rides the batch route, which
  already requires the owner node's bearer token, the current incarnation fence, a running
  session bound to that node and root, and an unexpired progress deadline. No route,
  credential, ticket or lease is added, and the `scan_library` lease still settles only after
  completion or failure. Publication events are appended per batch instead of in one completion
  burst; tickets still route work and events still schedule nothing (ADR 0001).
- **A partial session keeps what it published.** A session that fails, is cancelled, goes stale
  or meets a completion commit-lock conflict keeps the identities its accepted batches
  published, and retires nothing. Each is hash and probe evidence that agreed when its batch
  was accepted under the then-current owner, incarnation and root epoch, which the batch route
  checks. A session that later goes stale because the epoch or owner changed could never have
  completed, so its identities are true as of observation only; the next scan under the new
  epoch re-observes them and reconciles absence. No rollback or cleanup path exists.
- **No setting.** Batch-time publication replaces completion publication; completion keeps
  only the backstop above.

## Consequences

- On the measured corpus the first identity is durable about 11 s after the scan request
  instead of about 11 minutes.
- A plan made while a scan runs sees a partly refreshed root: some files carry this session's
  identity and snapshot (ADR 0131), the rest only earlier ones. Whether that is safe for
  planning and transforms is not claimed here; ADR 0120's transform-input full hash is the
  existing guard, and #841 did not exercise it.
- A failed session can leave new identities without a succeeded session. A reader must not
  treat "published" as "scanned by a completed session"; `last_scan_session_id` stays that
  signal. No reader makes the equation today.
- A completion commit-lock conflict no longer rolls publication back with it; only retirement
  waits for the retry.
- One writer transaction per batch replaces one large completion transaction: more, shorter
  write locks. On the measured corpus 34 batches cost nothing measurable on complete time;
  roots of many small files were not measured.
- Mixed versions publish everything once. An old agent's large batches publish when each is
  accepted, just later. A new agent against an old control plane publishes at completion as
  today. A control plane upgraded mid-scan publishes the old batches at completion through the
  backstop. A control plane downgraded mid-scan publishes at completion again, and the
  same-address path resolves locations an earlier batch already published to the same
  identity, recording one more media snapshot each.
- The 100,000-observation session ceiling and per-observation evidence bounds are unchanged.
- Not measured: the network corpus, probe-reuse-on cells, failure and restart paths, and
  concurrent planning during a scan.

## Considered & rejected

- **Keep completion-only publication.** verified: the stage-0 control's first durable
  observation equals complete time in every repetition (678.1 s median) and
  `published_before_complete` is 0, against 10.7 s per batch with no complete-time
  regression (`media-evidence-rollout.py compare`, records above).
- **Incremental flushes under completion publication.** verified: the control already flushed
  two batches per repetition and published nothing before completion; batch cadence cannot
  move a completion barrier.
- **Publish provisionally and roll back on session failure.** judgment: needs a provisional
  location state and a cleanup path, which ADR 0067 rejected, to undo identities that are
  already true.
- **Keep both modes behind a setting.** judgment: two publication contracts for one catalog,
  and the measurement gives no reason to keep the slower one.
