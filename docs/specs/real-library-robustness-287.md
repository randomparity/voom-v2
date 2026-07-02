# Real-library robustness fixes (T18 / #287)

Status: draft
Issue: #287 (closes #248, #249)
ADR: [0024](../adr/0024-malformed-media-and-hardlink-facts.md)

## Goal

A scan/execute over thousands of heterogeneous real files degrades **per-file,
never per-run, and reports honestly**. Three known sharp edges are addressed in
one PR:

- **(a) #248** — corrupt/malformed input classifies as the retriable
  `EXTERNAL_SYSTEM_UNAVAILABLE`, so a permanently-broken file looks retriable
  forever. Add a permanent `MALFORMED_MEDIA` failure class and have the
  ffprobe/ffmpeg workers emit it when the tool rejects the *input bytes*
  (as opposed to the tool itself failing transiently).
- **(b) #249** — hardlinked paths ingest as two independent assets with no
  inode facts, so mutation/commit can double-process one physical file. Record
  `st_dev`/`st_ino`/link-count as scan facts and use them so hardlinked paths
  resolve to one physical file (one `file_version`, two `file_locations`),
  distinct from a byte-identical copy (two physical files, two assets).
- **(c) mid-run visibility** — surface per-file progress counts
  (total/completed/failed/remaining) in the workflow summary and document the
  operator polling story for an in-flight run.

## (a) `MALFORMED_MEDIA` permanent failure class

### Taxonomy (`voom-core`)

- `FailureClass::MalformedMedia`, in the **non-retriable** partition
  (`retry_class` → `NonRetriable`, `is_retriable` → false; `High`
  severity/priority via the existing non-retriable derivation).
- `ErrorCode::MalformedMedia`, wire string `MALFORMED_MEDIA`.
- `FailureClass::MalformedMedia.into_error_code()` → `ErrorCode::MalformedMedia`;
  `FailureClass::from_error_code(ErrorCode::MalformedMedia)` →
  `Some(FailureClass::MalformedMedia)` (a clean 1:1 round-trip, unlike the
  `ProgressTimeout`/`WorkerTimeout` alias).
- No new `VoomError` variant: the class is produced by workers on the
  `ProgressFrame::Error` path (class + code), never constructed as a control-
  plane `VoomError`. The `voom-api` health `ErrorCode` match gains a
  `MalformedMedia` arm (maps to `INTERNAL_SERVER_ERROR`, the shared default
  group) purely to stay exhaustive.
- Conformance `failure_taxonomy.rs` registry gains one `MalformedMedia` entry
  (`PlannedCoverageSource::ChaosWorkerScenario`).

### Worker classification (the judgment-free part)

The distinction is **input-fault vs tool-fault**, decided by matching the
tool's own diagnostic text on a non-zero exit — deterministic string matching,
not a model call (AGENTS Rule 6). A non-zero exit whose stderr matches a curated
set of ffmpeg/ffprobe "the input is bad" signatures is `MalformedMedia`
(permanent); any other non-zero exit, a spawn failure, a signal kill, or a
timeout stays `ExternalSystemUnavailable` (transient). The signature set
(case-insensitive substring match):

```
invalid data found when processing input
moov atom not found
error opening input
could not find codec parameters
unknown format
invalid data found
end of file
partial file
header missing
error splitting the input into ndjson   (guard against false hit; excluded)
```

Concretely the matcher lives in a shared `is_malformed_media_stderr(&str) -> bool`
helper in each worker (they do not share a crate on this path). A signature
list that is *conservative* (high precision, lower recall) is correct here: a
missed malformed file falls back to the pre-existing retriable behavior (no
regression), while a false positive would wrongly mark a transient failure
permanent. Precision is therefore the priority.

- **ffprobe worker** (`ffprobe.rs`): the `!output.status.success()` arm
  currently always returns `external_system_unavailable("exit", …)`. It now
  branches: malformed-signature stderr → new `FfprobeError::MalformedMedia`;
  else unchanged. `failure_class()`/`error_code()` derive from the variant.
- **ffmpeg worker** (`ffmpeg.rs`/`handler.rs`): `FfmpegError` gains a
  `MalformedMedia(String)` variant. `command_error` classifies the process
  output; `run_ffmpeg_transcode`/`run_ffmpeg_command`/`probe_json` return
  `MalformedMedia` when the non-zero exit stderr matches. `TranscodeVideoError`
  gains a `MalformedMedia` variant → `FailureClass::MalformedMedia` /
  `ErrorCode::MalformedMedia`; `From<FfmpegError>` maps it through.

The `mkvtoolnix` remux worker has the same latent misclassification but is out
of scope for #287; flagged as a follow-up.

### Scan path

The directory-scan per-file failure path (`scan/mod.rs`
`ScanCandidateOutcome::WorkerError`) currently continues past a probe failure
only when `error.is_ffprobe_exit()` (an `ExternalSystemUnavailable` at the
`exit` stage). A `MalformedMedia` terminal is likewise a per-file fault the
directory scan must survive: `is_ffprobe_exit` is widened (renamed
`is_unprobeable_media`) to also admit `ErrorCode::MalformedMedia`, so a corrupt
file in a large directory scan records a per-file failure and the run continues
(matching #213 behavior), now with the honest permanent class.

## (b) hardlink inode facts (#249)

### Capture

`scan::hash::observe_candidate_file` already stats the opened file. Add
`st_dev`, `st_ino` (Unix `MetadataExt::dev()/ino()`) and `nlink()` to
`ObservedFileFacts` as `Option<u64>` (None on non-Unix / stat miss). No new
syscall.

### Persist (migration 0017)

New additive table `scan_file_facts`:

```sql
CREATE TABLE scan_file_facts (
    file_location_id INTEGER PRIMARY KEY
        REFERENCES file_locations(id) ON DELETE CASCADE,
    dev              INTEGER NOT NULL,
    ino              INTEGER NOT NULL,
    nlink            INTEGER NOT NULL,
    observed_at      TEXT NOT NULL
);
CREATE INDEX idx_scan_file_facts_dev_ino ON scan_file_facts(dev, ino);
```

One row per ingested local `file_location`, keyed 1:1 by that location. The
`(dev, ino)` index is the hardlink lookup. `nlink` is recorded for operator
visibility / future use (a value > 1 means the physical file has other links).

A small additive repo `SqliteScanFactsRepo` (new file under
`voom-store/src/repo/`) exposes:

- `record_in_tx(tx, file_location_id, dev, ino, nlink, observed_at)`
- `find_live_location_by_dev_ino_in_tx(tx, dev, ino) -> Option<ScanFactMatch>`
  — joins `file_locations` (live, `kind='local_path'`) to return the prior
  location id + its `file_version_id` for the same `(dev, ino)`.

### Resolution (scan persist)

In `persist_scanned_media_snapshot`, when Unix inode facts are present:

1. Look up `find_live_location_by_dev_ino_in_tx(dev, ino)`.
2. **Match** → the physical file is already ingested under a prior path. Attach
   the new path as an additional live `file_location` on the *existing*
   `file_version` and record a `scan_file_fact` for it. No new `file_asset` /
   `file_version` / snapshot. `PersistedScan` points at the existing
   asset+version with the new location id, and is reported with a new
   `ScanReportFileStatus::ScannedHardlink` (still counts as a scanned file, but
   `ingested` is not double-incremented — it is a new *location* on an existing
   asset, so `hardlinked` is counted separately in the summary).
3. **No match** → ingest as today, then record a `scan_file_fact` for the new
   location.

The attach reuses the existing alias machinery's protection: it routes through
a new `IdentityRepo::attach_local_hardlink_location_in_tx` that inserts the
location and, like the alias-attach path, consults the pending-commit lock so a
hardlink attach cannot race past the commit safety gate's authorized closure.

Hardlink vs copy is distinguished exactly: a hardlink shares `(dev, ino)`; a
byte-identical copy has the same content hash but a different `ino`, so it takes
the no-match branch and stays a distinct asset (its content-hash match is
already surfaced as `hash_match` evidence, unchanged).

The `#[ignore]` chaos e2e characterization
(`hardlinked_paths_scan_as_duplicate_candidates_with_shared_hash`) is rewritten
to assert the new behavior (one asset, two locations). It requires external
Chaos Librarian tooling and does not run in normal CI; a store-level
integration test in `voom-store` and a control-plane persist test provide the
CI-gating coverage.

## (c) mid-run visibility

### Progress counts in the workflow summary

`WorkflowSummaryView` (`cases/policy/compliance.rs`) gains
`progress: ProgressCountsView { total, completed, failed, remaining }`, derived
purely from the run's `file_phases` (not from the job counters). Definition,
computed per distinct `branch_id` by its **latest** (highest `phase_ordinal`)
file-phase row:

- `total` — distinct files (branch ids) the run has touched.
- `completed` — files whose latest outcome is `committed`.
- `failed` — files whose latest outcome is `blocked`.
- `remaining` — `total − completed − failed` (files whose latest outcome is
  `skipped`, i.e. deferred/not-yet-terminal at the frontier).

Computed by a pure `progress_counts(&[FilePhaseSummaryView]) -> ProgressCountsView`
so it is unit-testable without a DB. Both construction sites — `execute`
(`ComplianceExecuteData::from_outcome`) and `report --job-id`
(`read_compliance_run_report`) — build the view from their already-materialized
`file_phases`, so a poller sees identical progress. The `From<&WorkflowSummary>`
impl is replaced by `WorkflowSummaryView::from_summary(summary, &file_phases)`.

Because a new field is added to a serialized CLI type, the affected `insta`
snapshots are regenerated and reviewed in the same change.

### Runbook

`operator-real-media-execution.md` gains a "Mid-run monitoring" section: while a
`compliance execute` is in flight, WAL permits concurrent reads, so an operator
can poll `voom compliance report --job-id <id>` for the progress counts and
per-file phase outcomes, and `voom worker list` for live workers. Concurrent
reads during a run are test-verified (`operator_execution_e2e.rs`). Output-tree
naming at scale (#197/#199) is already documented and unchanged.

## Non-goals

- mkvtoolnix remux worker malformed-media classification (follow-up).
- Cross-device hardlink emulation, reflinks/CoW, or content-defined dedup.
- Live progress streaming; polling a WAL reader is the mechanism.

## Success criteria

- New non-retriable `MALFORMED_MEDIA` class + code round-trip; ffprobe/ffmpeg
  emit it on corrupt input and `ExternalSystemUnavailable` on transient tool
  failure, each covered by a test that fails if the branch is swapped.
- Two hardlinks resolve to one `file_asset` + one `file_version` with two live
  `file_locations` and two `scan_file_facts`; a byte-identical copy stays two
  assets. Covered by CI-gating store + control-plane tests.
- `WorkflowSummaryView.progress` counts are correct for a mixed
  committed/blocked/skipped frontier and identical between `execute` and
  `report --job-id`; runbook documents the polling story.
- `just ci` green; migration 0017 applied only via `voom init`.
