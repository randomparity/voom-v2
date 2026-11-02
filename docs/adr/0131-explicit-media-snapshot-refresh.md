# 0131 — Explicit scans append media snapshots for unchanged files

## Status

Accepted (2026-11-01). Implemented by #825.

## Context

ADR0125 records that older MP4 cover snapshots lack `attached_pic`. The owner scan
already probes unchanged primaries, but completion discards that evidence when identity
resolves to an existing version. Normalized provider facts can change independently of
content identity. Planning cannot read `raw.ffprobe_json` (ADR0122).

## Decision

A successful explicit owner root scan appends a media snapshot and its recorded event
for each identity-agreed evidence-bearing primary observation, after resolving its file
version and inside the existing completion transaction. Reuse the current snapshot/event
helper. Isolate publication and store completion in the existing savepoint helper, so
completion rejection rolls back domain writes before the outer transaction commits its
error replay. Preserve validation and event order. Unchanged bytes retain their file version. Same-content rescans and hardlink
observations each retain their own probe observation, even if payloads are equal.

New root/whole-scan policy inputs use the greatest snapshot ID as existing readers do.
Existing stored input sets, generated plans and operations retain their snapshot IDs
and payloads. Newly generated planning from stored inputs retains the existing active
version/latest-snapshot projection; no historical operation pin is replaced. Operators
rescan, then create new plans to consume refreshed provider facts.
No automatic refresh, new CLI selection surface or control-plane media reads are added.

## Consequences

- Legacy cover facts become available through `voom scan --root <storage-root-id>`.
- Snapshots grow by one row per evidence-bearing observation; old rows remain available.
- Evidence-less or failed scans cannot refresh snapshots; existing snapshots remain.
- Atomic completion and idempotent replay govern refreshed rows and events as for fresh ingest.
- A historical plan retains its historical limitations and may need replacement.

## Considered & rejected

- **Overwrite old snapshots or retarget plans.** judgment: destroys the chosen immutable
  history and pin semantics.
- **Create a new file version by touching unchanged files.** judgment: conflates content
  identity with provider projection and violates the approved unchanged-version contract.
- **Add a dedicated refresh worker/API.** verified: at `ade82d56`, the owner scan pipeline
  `scan_session.rs::process_candidate` already hashes and probes each discovered primary;
  `scan/publish.rs::publish_one` discards that snapshot only on existing identity branches.
  A second execution path duplicates work already done.
- **Compare payloads and skip equal observations.** judgment: adds a read/comparison and a
  distinct history rule without being needed for the requested explicit refresh.
- **Plan from raw JSON.** verified: ADR0122 forbids this and can omit raw above the bound.
- **Do nothing.** verified: ADR0125's Consequences identifies legacy MP4 covers that remain
  incorrectly classified without a re-probe/backfill.
