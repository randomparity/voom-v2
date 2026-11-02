# Explicit media snapshot refresh (#825)

## Problem

An owner scan hashes and probes each discovered primary, but completion records a
snapshot only for a fresh identity. Same-address unchanged files and newly discovered
hardlinks reuse the version and discard their probe result. Consequently, normalized
facts added by a newer provider never reach old files through an explicit rescan.
ADR0125's MP4 cover is the concrete regression; ADR0122 forbids planning from raw JSON.

## Authority and scope

The frozen charter is issue825's WORK:SCOPE token `q825-35333637`, campaign
`eea194a5a966-6c546866-0556-4c27-aee2-0e72f70f565c`. The operator approved explicit
rescan refresh, unchanged content identity, immutable historical snapshots and existing
pins on 2026-11-01; exclusions are EMPTY. C1–C4 refer to that charter unchanged.
This is one bounded extension of the existing owner scan publication boundary.
No new owner, selection API, event type, worker operation, schema or migration is needed.

## Design

The existing operator command `voom scan --root <storage-root-id>` remains the entry
point. It schedules a scan ticket; the owner agent and out-of-process workers obtain
identity-agreed bounded probe evidence. The control plane reads no media bytes.

`scan/publish.rs::publish_one` first resolves same-address identity, hardlink identity
or new ingest exactly as today. After that resolution, it calls the existing
`media_snapshot::record_with_event_in_tx` once for that evidence-bearing observation.
Move the existing snapshot block out of `ingest_new_scanned_file`, remove the now-unused
snapshot field from `NewScanIdentity`, and normalize stream IDs through the existing
normalizer. No duplicate fresh-ingest insertion remains. Preserve `probed_by: None`
and completion timestamp semantics; per-file provenance still resides in scan evidence.

This inserts a fresh snapshot row even when the normalized payload equals its predecessor.
A scan is a new observation, independent of whether the file-version content changed.
For a hardlinked pair, each evidence-bearing address publishes a snapshot on their shared
version, in the existing deterministic observation order; newest means greatest snapshot
ID, matching existing readers. Identity rows and version epochs remain unchanged.

The snapshot insert and recorded event occur after identity/inode facts and before sidecar
membership, retirement, scan success and idempotency completion, inside the existing
caller-owned completion transaction. Fresh-ingest ordering is preserved.
Publication and store completion run under the existing `voom_store::tx::begin_savepoint`
inside that transaction. On a store completion error, explicitly roll back the savepoint
before preserving the existing outer error-replay/commit-lock routing. A publication
error still returns directly and rolls back the outer transaction as before. On success,
release the savepoint, then append completion success/replay and commit the outer transaction.
This matches `artifact/commit/intent.rs`'s existing mutation-before-error-replay pattern;
no new transaction API is introduced. Earlier snapshots remain intact on either failure.
Existing completion preflight fences and exact request/terminal replay precede publication;
a replay cannot mint another snapshot. Evidence-less observations protect existence and
publish no identity or snapshot, so a failed probe leaves the prior snapshot usable.

New whole-scan and root-scoped policy input capture already choose the greatest snapshot
ID; explicit selected-file input capture still uses the supplied snapshot ID. Existing
input sets, generated plans and media operations keep their recorded IDs and stream summaries.
New planning from a stored input deliberately reprojects the current active version/latest
snapshot in `resolve_stored_planning_input`; retain that existing behavior. An old input's
stored pin is history, not a request to force its facts into future newly generated plans.
An already planned operation resolves its explicit snapshot ID through `snapshot_for`;
refresh must not retarget that pin. The store remains owner of SQL/checked row
decoding; scan publication remains owner of transaction/event composition.

See [ADR0131](../../adr/0131-explicit-media-snapshot-refresh.md).

## Success

- C1: A second successful owner root scan of unchanged synthetic media appends normalized
  facts to the same version. A legacy MP4 cover obtains `attached_pic` without touching bytes.
- C2: Earlier snapshot payloads and input-set/plan pins remain byte-for-byte unchanged;
  new root/whole-scan input sets select the refreshed ID and count only the real video.
- C3: Fresh ingest still produces one snapshot per evidence observation; hardlink identity
  is shared; malformed normalized input, event abort, later completion abort, stale fences, wrong final watermarks
  and exact replay preserve atomicity and their existing ordering.
- C4: A fresh application database and authenticated CLI/API/owner stack scan a synthetic
  MP4 cover, simulate a legacy snapshot only in that fresh DB, rescan unchanged bytes,
  create new inputs/plan and successfully remux the selected source to a separate output.
  Source hash/size/mtime and version identity remain stable. No corpus or legacy DB is used.

## Failure model

- Actors and deployments: local operators running explicit scans against registered,
  activated owner nodes; CLI/API/agent processes; Linux and macOS project test deployments.
- Invariants/assets: immutable snapshot history and pins; exact content identity; stale
  owner/root/session fences; atomic mutation/event order; bounded owner-side probe evidence.
- Accepted failure classes: failed or evidence-less probes cannot refresh facts (existing
  scan failure/degradation contract); an existing old plan may still fail for its old MP4
  facts (operator chose immutable pins); repeated scans increase snapshot storage (one row
  per evidence observation; no automatic periodic refresh is introduced).
- Covered elsewhere: evidence size/raw omission ADR0122; hashing/reuse and owner byte access
  ADR0120/ADR0077; scan absence reconciliation and incarnation/root fencing existing sessions;
  corrupt persisted identity/JSON decoding voom-store.

## Threat model

- Boundary inventory: no new entry point; existing owner evidence can now append facts to
  an existing version as well as a newly ingested one. Persisted snapshot JSON stays opaque.
- Actor model: authenticated configured owner may submit media facts; media content and
  persisted SQLite rows are untrusted. No protection from a malicious authorized owner is added.
- Controls: existing scan authority/epoch/incarnation/deadline preflight and identity hash/size
  agreement precede publication; batch evidence size/shape checks remain; existing normalizer
  assigns missing stream IDs and rejects nonobjects/missing numeric indices; store checked decoding and event helper propagate errors atomically.
- Outside this change: compromised owner correctness is the existing trust model; byte
  mutation races and probe limits retain the existing worker/hash and scan fences.

## Global Constraints

Rust toolchain 1.95.0; no new dependencies or protocol/schema changes. Preserve domain
newtypes, caller-owned named transactions, durable strict serde units and deterministic
query ordering. Unit tests use sibling files and real time with ManualClock for DB work.
All routine guards use just; hooks are mandatory; final just ci runs on the actual final base.
Only synthetic media and fresh application databases may be used for runtime verification.

## Validation

Run `cargo test -p voom-control-plane --lib scan::` for publication, completion,
replay, fences and rollback. Run the focused `stored_stream_input_replaces_cached_facts`
and `snapshot_for_keeps_pin_after_refresh` control-plane tests for latest planning and
historical dispatch pins. `cargo test -p voom-cli --test media_snapshot_refresh` exercises
the authenticated synthetic owner rescan, root/whole input capture and committed remux.
Use freshly built current-source binaries. Controlled event omission, savepoint commit
instead of rollback, and latest lookup instead of explicit pin must each fail their
focused regression; restore the source and require green afterward. Run relevant just
guards, lint, mandatory hooks and final `just ci` before shipping.
