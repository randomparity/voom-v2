---
name: voom-sprint-10-design
description: Sprint 10 design for explicit-path real media scan, hashing/location ingest, and an out-of-process ffprobe worker.
status: proposed
date: 2026-05-24
sprint: 10
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-24-voom-sprint-9-design.md
  - docs/superpowers/specs/2026-05-24-voom-sprint-9-closeout.md
---

# VOOM Sprint 10 - Real Ingest And FFprobe Worker

## 1. Purpose

Sprint 10 introduces the first real media input path. A user can point the CLI
at an explicit file or directory, VOOM discovers supported media files, hashes
and records their locations, probes them through a real out-of-process
`ffprobe` worker, and persists media snapshots that later policy and planning
sprints can consume.

This sprint is read-only. It does not mutate media files, stage artifacts,
transcode, remux, back up originals, run a daemon watch loop, transfer media to
remote nodes, or introduce durable library roots. Durable library roots are a
future scan configuration task after explicit-path ingest proves the data model
and provider boundary.

## 2. Scope

Sprint 10 delivers:

- `voom scan --path <file-or-dir>` as the only scan entry point.
- Local explicit-path discovery for supported media files.
- Deterministic local hashing and observed file facts for discovered files.
- Ingest through the existing identity model: file assets, file versions, file
  locations, and ingest evidence already supported by `IdentityRepo`.
- A real out-of-process `ffprobe` worker for `probe_file`.
- A typed Sprint 10 probe request and probe result payload over the existing
  worker protocol.
- Normalization from `ffprobe` JSON into a stable Sprint 10 media snapshot
  payload.
- Media snapshot persistence linked to the ingested file version and probing
  worker.
- Agent-facing scan summary JSON fixtures and CLI golden tests.
- Small fixture-media integration tests that exercise real `ffprobe` in the
  release verification environment.
- Closeout documentation tying scan, ingest, snapshot, and provider-boundary
  behavior to repeatable evidence.

Sprint 10 explicitly does not deliver:

- Durable library roots, library-root policies, or scheduled scans.
- File-system watch mode or daemon scan loops.
- Remote media transfer or remote `ffprobe` execution.
- Staged artifact mutation, verification, commit, rollback, remux, transcode,
  backup, or delete behavior.
- External-system refresh or library sync.
- Policy-driven scan selection.
- Broad REST or Web UI scan surfaces.
- Automatic identity merge beyond existing ingest evidence semantics.

## 3. Architecture

The Sprint 10 scan path is:

```text
voom scan --path <file-or-dir>
  -> local explicit-path validation
  -> local recursive discovery for directories
  -> local size/hash/stat collection for the candidate bytes
  -> out-of-process ffprobe worker dispatch for probe_file with expected facts
  -> worker verifies and returns observed file facts with the probe result
  -> control plane revalidates observed facts against the candidate bytes
  -> one transaction records discovered file and media snapshot
  -> one CLI JSON envelope with scan summary and per-file results
```

`voom-cli` owns command parsing and envelope emission. `voom-control-plane`
owns scan orchestration, path validation, summary construction, and repository
transactions. `voom-store` owns identity and snapshot persistence. The
`ffprobe` worker owns shelling out to `ffprobe`, parsing its JSON, and returning
a typed worker-protocol result. The worker must not write the VOOM database or
apply ingest decisions itself.

Sprint 10 uses a bundled local worker lifecycle, not daemon discovery. For
`voom scan`, the control plane launches the bundled `ffprobe` worker as a child
process over loopback or an equivalent local worker-protocol transport, performs
the worker handshake, and dispatches `probe_file` requests through that
transport. Before the first dispatch, the control plane must ensure exactly one
live durable local worker row exists for this bundled worker with `probe_file`
capability and an execution grant. The stable bootstrap identity is the worker
name `builtin.ffprobe`; if a live worker with that name already exists, scan
reuses it, otherwise scan creates it with `kind = local` and `node_id = NULL`.
The worker id from that row is the value recorded in `media_snapshots.probed_by`
and returned in the CLI envelope.

The bundled worker bootstrap is idempotent and system-owned. Repeated scans must
not create one worker row per invocation. The bootstrap must not require the
user to run `voom node register` or `voom worker register` before a local scan,
and it must not introduce remote node registration, production TLS, token
rotation, or daemon supervision into Sprint 10. It also must not use an
in-process `ffprobe` shortcut; process separation is part of the acceptance
surface.

The local discovery and hashing steps intentionally run in the control-plane
process for Sprint 10. They are deterministic local file-system reads, not
media-tool execution. The provider boundary is preserved for media probing:
`ffprobe` is invoked only by the out-of-process worker, using the same
versioned protocol boundary as synthetic providers.

Per-file persistence is atomic for successful probes: the file asset/version,
file location, and media snapshot are recorded in one transaction after the
worker result has passed the content-consistency gate. Probe failures do not
create a successful media snapshot. Sprint 10 may record an issue or event for
failed probes if that is already available through existing surfaces, but it
must not leave a file-version row that the CLI reports as successfully scanned
without a corresponding snapshot.

Events may record scan and snapshot facts, but durable identity tables and
`media_snapshots` remain the source of truth. Events do not route scan work.

## 4. CLI Contract

Sprint 10 adds:

```text
voom scan --path <path>
```

The command accepts exactly one explicit path. If the path is a file, the
command scans that file. If the path is a directory, the command recursively
discovers supported media files under that directory using deterministic
lexicographic ordering by normalized path.

The command emits exactly one JSON envelope on stdout. The `data` object must
include at least:

```json
{
  "path": "/absolute/input/path",
  "mode": "file|directory",
  "summary": {
    "discovered": 1,
    "ingested": 1,
    "probed": 1,
    "snapshots_recorded": 1,
    "skipped": 0,
    "failed": 0
  },
  "files": [
    {
      "path": "/absolute/input/path/movie.mkv",
      "status": "scanned",
      "file_asset_id": 1,
      "file_version_id": 1,
      "file_location_id": 1,
      "media_snapshot_id": 1,
      "content_hash": "blake3:...",
      "size_bytes": 1234,
      "probe_worker_id": 7
    }
  ],
  "skipped": []
}
```

When a file status is `failed` or `failed_content_drift`, the file object must
include an `error` object:

```json
{
  "path": "/absolute/input/path/movie.mkv",
  "status": "failed_content_drift",
  "error": {
    "code": "ARTIFACT_CHECKSUM_MISMATCH",
    "failure_class": "artifact_checksum_mismatch",
    "message": "file changed between hashing and probing"
  }
}
```

Stable file result statuses:

```text
scanned
skipped_unsupported_extension
failed_content_drift
failed
```

Unsupported files discovered during directory recursion are reported in
`skipped` and may also increment `summary.skipped`. An explicitly provided file
with an unsupported extension is a `BAD_ARGS` command failure because the user
selected that file directly.

## 5. Discovery And Hashing

Sprint 10 supports a conservative extension allowlist for files that will be
sent to `ffprobe`:

```text
.avi .m2ts .m4v .mkv .mov .mp4 .mpeg .mpg .ts .webm
```

The allowlist is a scan optimization and CLI contract, not a statement of all
future supported media types. The design favors a small explicit list so
directory scans do not send obvious sidecars, subtitles, images, or arbitrary
large files to `ffprobe`.

Discovery rules:

- Reject an explicit symlink with `BAD_ARGS` before canonicalization.
- Canonicalize the requested path before scanning.
- Reject missing paths.
- Reject symlink traversal in Sprint 10. Symlink policy belongs with durable
  library roots because roots will need explicit boundary rules.
- Traverse directories deterministically.
- Ignore directories or files that become unavailable during traversal only
  when they were not the explicit path; report them as skipped or failed in the
  summary.
- Fail if the explicit path cannot be read.

Hashing uses BLAKE3 and persists the same content-hash string vocabulary already
used by ingest fixtures. Hashing and stat collection happen before database
ingest so `record_discovered_file_in_tx` receives stable observed bytes.

The candidate file facts sent to the worker are mandatory for successful media
snapshot persistence:

- canonical local path;
- size in bytes;
- BLAKE3 content hash;
- modification timestamp when the platform exposes one;
- best-effort device/inode or equivalent stable local file key when available.

The `ffprobe` worker must verify the expected size and BLAKE3 hash immediately
before invoking `ffprobe`, then verify size and BLAKE3 hash again immediately
after `ffprobe` exits. The worker returns both pre-probe and post-probe observed
facts. The control plane records a snapshot only when both worker observations
match each other and match the candidate facts used for ingest. If they differ,
the file is reported as `failed_content_drift`; the implementation may retry
once, but it must never persist a media snapshot for bytes that differ from the
ingested `file_versions.content_hash`.

Sprint 10 records `local_path` file locations. Local physical-object proof is
best-effort and platform-dependent; if the existing identity proof model cannot
capture a reliable proof portably in this sprint, the ingest path may leave
`proof_kind` and `proof_value` empty. That does not weaken Sprint 10's
acceptance criteria because rename reconciliation is not part of this sprint.

## 6. FFprobe Worker

The worker should be a real binary, not a test fake. The implementation plan may
choose the final crate name, but the design expectation is a media-worker crate
or binary such as `ffprobe-worker`. Production `voom scan` must launch this
binary out of process and dispatch over the worker protocol.

The worker:

- registers or advertises `probe_file`;
- accepts a typed probe request containing the local file path and mandatory
  expected size/hash metadata;
- verifies the requested path is still a regular file with the expected size
  and BLAKE3 hash before invoking `ffprobe`;
- invokes `ffprobe` with JSON output;
- verifies the regular file, size, and BLAKE3 hash again after `ffprobe`
  exits;
- parses the returned JSON;
- emits structured progress frames using the existing worker protocol;
- returns a typed probe result containing normalized container, stream, format,
  duration, bitrate, codec, dimensions, frame-rate, audio, subtitle, and raw
  provenance fields needed for debugging.

The worker must fail loudly when:

- `ffprobe` is not found: `external_system_unavailable`;
- `ffprobe` exits non-zero: `external_system_unavailable`;
- output is not valid JSON: `malformed_worker_result`;
- required fields for the Sprint 10 snapshot shape cannot be interpreted:
  `malformed_worker_result`;
- the probed file no longer matches the expected size/hash:
  `artifact_checksum_mismatch`.

The worker must not directly access SQLite, create file assets, or record media
snapshots. Its only durable effect is through the control plane consuming its
result.

## 7. Snapshot Payload

Sprint 10 stores a versioned media snapshot payload in `media_snapshots.payload`
with this shape:

```json
{
  "format": "sprint10-v1",
  "probe": {
    "provider": "ffprobe",
    "provider_version": "7.0",
    "command": "ffprobe",
    "probed_at": "2026-05-24T00:00:00Z"
  },
  "container": {
    "format_name": "matroska,webm",
    "format_long_name": "Matroska / WebM",
    "duration_seconds": 12.34,
    "bit_rate": 1234567
  },
  "streams": [
    {
      "index": 0,
      "kind": "video",
      "codec_name": "h264",
      "width": 1920,
      "height": 1080,
      "duration_seconds": 12.34,
      "avg_frame_rate": "24000/1001"
    }
  ],
  "raw": {
    "ffprobe_json": {}
  }
}
```

`raw.ffprobe_json` may be preserved for operator diagnostics, but policy and
planning code must consume the normalized fields. The normalized shape should
use absent JSON fields for unknown values instead of sentinel strings. Numeric
conversion must reject values that do not fit the chosen types.

The stored `media_snapshots.probed_by` field records the worker id when the
scan is dispatched through the bundled registered local worker. Tests may use a
short-lived worker process, but they must still create or reuse the durable
worker row whose id is persisted on the snapshot. A snapshot with
`probed_by = NULL` is not acceptable for Sprint 10 CLI scan output.

## 8. Persistence And Idempotency

The ingest behavior should reuse existing identity repository semantics:

- a new content hash records a new file asset, file version, and file location;
- matching content hash records hash-match evidence instead of merging assets;
- existing alias/rename reconciliation behavior is left unchanged;
- media snapshots are append-only facts tied to file versions.

Sprint 10 does not need full scan idempotency. Re-running `voom scan --path` may
record another media snapshot for the same file version if the file remains
unchanged. The CLI summary should make created row ids visible so agents can
inspect the resulting state.

If implementation discovers that duplicate location insertion would make
repeat scans noisy or misleading, the plan may add a narrow repository helper
that reuses an existing live `file_locations` row for the same
`file_version_id`, `kind`, and `value`. That helper must not merge logical file
assets or alter existing identity-evidence semantics without a separate design
update.

## 9. Error Handling

Stable command failures:

- missing explicit path: `BAD_ARGS`;
- unsupported explicit file extension: `BAD_ARGS`;
- unreadable explicit file: runtime error;
- hashing failure: runtime error;
- ingest persistence failure: runtime error;
- worker launch/dispatch failure: runtime error;
- `ffprobe` unavailable: runtime error with
  `external_system_unavailable`;
- non-zero `ffprobe` exit: runtime error with
  `external_system_unavailable`;
- invalid `ffprobe` JSON: runtime error with `malformed_worker_result`;
- content drift between hashing and probing: runtime error with
  `artifact_checksum_mismatch` and `failed_content_drift` in the per-file
  failure payload.

Directory scans may complete with warnings for unsupported extensions and files
that disappear before scanning. A selected media file that fails hashing,
ingest, worker dispatch, or probing causes the command to fail and reports the
per-file failure in the error payload. Silent skips are not allowed.

The scan result must not claim success if any media file selected for probing
failed. If a command fails after some earlier files were successfully committed,
the error payload must include both committed successes and the failing file so
an agent can inspect durable state without guessing whether the command was
all-or-nothing.

## 10. Testing

Required tests:

- discovery unit tests for explicit file, directory recursion, deterministic
  ordering, unsupported extension handling, and symlink rejection;
- hash and observed-file-fact tests;
- `ffprobe` JSON normalization unit tests using checked-in JSON fixtures;
- worker conformance tests for successful `probe_file`, missing `ffprobe`,
  non-zero exit, invalid JSON, and expected-size/hash drift;
- local worker bootstrap tests proving `voom scan` reuses the
  `builtin.ffprobe` worker row, does not require manual node or worker
  registration, and does not use an in-process `ffprobe` shortcut;
- content-drift tests proving a changed file cannot produce a media snapshot
  bound to the stale ingested hash;
- control-plane integration tests proving file asset, file version, file
  location, and media snapshot persistence are atomic for successful probes;
- CLI golden-output tests for scan success and representative failures;
- small fixture-media integration tests that run real `ffprobe`;
- documentation placeholder scan;
- `just ci`.

The implementation plan should make fixture-media tests deterministic. Release
verification requires `ffprobe`; tests that require the binary must fail loud in
that path rather than being silently skipped. Unit tests for normalization can
still run without the binary by using JSON fixtures.

## 11. Project Spec Update

Sprint 10 updates `docs/specs/voom-control-plane-design.md` so the roadmap is
explicit:

- Sprint 10 scan is explicit-path only.
- Durable library roots are deferred to a future task after explicit ingest.
- The `ffprobe` worker is part of the Sprint 10 release scope.

This keeps the roadmap from implying durable scan configuration before the
first real ingest path is proven.

## 12. Acceptance Criteria

Sprint 10 is complete when:

- `voom scan --path <fixture-dir>` emits one valid JSON envelope.
- The scan persists file assets, file versions, file locations, content hashes,
  and media snapshots for supported fixture media.
- The media snapshot is produced from a real out-of-process `ffprobe` worker.
- Repeated scans reuse the durable `builtin.ffprobe` worker row instead of
  creating a new worker per scan.
- The CLI reports skipped unsupported directory entries without silently
  ignoring selected media files.
- Worker conformance tests cover success and failure cases.
- CLI golden tests lock the scan envelope shape.
- The architecture spec names explicit-path scan scope and defers durable
  library roots.
- The Sprint 10 closeout matrix records repeatable evidence for scan, ingest,
  snapshot, and provider-boundary behavior.
- `just ci` passes.
