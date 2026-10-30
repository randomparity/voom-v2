# Oversized probe results degrade one file, not the scan session (#816)

Decision record: [ADR 0122](../../adr/0122-bounded-probe-snapshot.md).

## Problem

`NdjsonReader` rejects any frame whose payload exceeds 64 KiB
(`crates/voom-worker-protocol/src/wire/ndjson.rs:26`). The `probe_file` result embeds the full
ffprobe JSON (`crates/voom-ffprobe-worker/src/normalize.rs:53`). For the two densest
local-corpus files (58 and 64 streams), the result frame is about 73 and 80 KB. The scan
pump's `dispatch_until_terminal` turns the reader error into `PumpFailure(WorkerCrash)`, so
the whole session fails and publishes nothing. Even with a larger cap, `process_candidate`
would degrade the observation to evidence-less, because its evidence exceeds
`MAX_EVIDENCE_BYTES` (`scan_session.rs:917`).

Measurements use ffprobe 8.1.3 on the two failing files, through `normalize_ffprobe_json` at
f8be728c. Sizes are compact JSON bytes:

| file | streams | raw ffprobe JSON | snapshot today | snapshot without raw |
|---|---:|---:|---:|---:|
| f0010 | 58 | 59,726 | 72,618 | 12,868 |
| f0012 | 64 | 64,547 | 79,507 | 14,936 |

Normalized facts run about 230 bytes per stream; raw is 80% of the snapshot. f0011 (49 streams,
a 64,143-byte frame today) also crosses the new 60 KiB bound and loses `raw`.

## Design

### Protocol constants (`voom-worker-protocol`)

- `wire/ndjson.rs`: rename the private `DEFAULT_MAX_FRAME_BYTES` to
  `pub const MAX_FRAME_BYTES: usize = 64 * 1024`, exported from `lib.rs`. The value and
  reader behavior are unchanged.
- `operations/probe_file.rs`: `pub const MAX_PROBE_SNAPSHOT_BYTES: usize = 60 * 1024`,
  documented as the producer's bound on `ProbeFileResult.snapshot` serialized as compact
  JSON. It is exported from `lib.rs`.
- The 4 KiB of headroom covers the frame envelope and facts, and the evidence fields, with
  room for sidecars. Two tests in `probe_file_test.rs` pin it with worst-case values:
  `u64::MAX` sizes and seq, nanosecond RFC 3339 timestamps (a fixed one for `emitted_at`), a 64-character provider
  version, a full `FileKeyFacts`, `hash_source: reused`, and a snapshot of exactly
  `MAX_PROBE_SNAPSHOT_BYTES`.
  1. The serialized `ProgressFrame::Result` is ≤ `MAX_FRAME_BYTES`.
  2. A `ScanObservationEvidence` with no sidecars is ≤ `MAX_EVIDENCE_BYTES`, and passes
     `validate()`.

### Probe worker (`voom-ffprobe-worker`)

- `normalize_ffprobe_json` builds the snapshot as it does today, then calls
  `bound_snapshot(snapshot, raw)`:
  1. Insert `raw.ffprobe_json`. If the compact length is ≤ the bound, return the snapshot.
  2. Otherwise set `raw` to `{"ffprobe_json_omitted_bytes": <compact length of the raw JSON>}`.
     If the length is now ≤ the bound, return it.
  3. Otherwise return `WorkerError::MalformedMedia("probe snapshot is <n> bytes without raw
     ffprobe JSON, above the <bound>-byte bound")`.
- `WorkerError::MalformedMedia` is new and maps to `FailureClass::MalformedMedia`. In
  `handler.rs`, `From<WorkerError> for FfprobeError` gains one arm to
  `FfprobeError::MalformedMedia` with stage `normalize_snapshot`. Only that `impl` changes,
  so the `scripts/media-evidence-candidates.py` anchors (import, pre/post reads,
  `verify_expected_facts`) are untouched.
- The guarded path (`guard.rs`) already calls `normalize_ffprobe_json`, so it is bounded
  with no change.

### Node agent

There is no code change. `classify_single_file_error` already maps
`(MalformedMedia, _)` to `UnusableIdentity`, which gives an evidence-less observation
(`scan_session.rs:643-648`). A new pump test pins the per-file outcome.

### Sibling sweep

- Every `probe_file` reader gets the bounded result: the scan pump, node-agent output probes
  (`media.rs`), and control-plane workflow dispatch (`workflow/execution/dispatch.rs`).
- Other workers do not embed tool JSON in results. `voom-ffmpeg-worker::probe_json` and
  `voom-mkvtoolnix-worker::identify_json` reduce it to per-stream facts.
- `OwnerObservedOutput` (`publication.rs`) copies the bounded snapshot into an HTTP body that
  the API's ~1 MiB cap governs, not the frame cap.
- Readers of the `raw` shape are CLI test support only (`observed_state.rs`,
  `structural_matrix.rs`). They see an oversized file as missing raw evidence (ADR 0122).
- The scan pump drops its private `MAX_EVIDENCE_BYTES` and `MAX_SIDECAR_ENTRIES` copies for
  `voom_core::{MAX_EVIDENCE_BYTES, MAX_EVIDENCE_SIDECARS}`, so the evidence pin covers the
  value it enforces.

## Failure model

1. **Actors and deployments:** an owner-node agent scanning a local or network library,
   with its bundled ffprobe worker. A library file's container metadata is untrusted input.
2. **Invariants and assets at stake:** a probe result too large for one frame must not fail
   a scan session (ADR 0077 completion-gated publication); readers keep their `MAX_FRAME_BYTES`;
   the evidence budget is unchanged; normalized snapshot facts are never truncated.
3. **Accepted failure classes:**
   - A file whose normalized snapshot alone exceeds 60 KiB is scanned without identity and
     labelled `MalformedMedia`. Its cost is bounded to that file.
   - Evidence with many sidecars plus a near-bound snapshot can still exceed 64 KiB and
     degrade to evidence-less, as it does today (ADR 0077).
   - `provider_version` length comes from the operator-installed ffprobe binary, which is
     trusted, and is not bounded here.
   - Content-driven normalizer errors that remain `MalformedWorkerResult` (for example,
     conflicting compatibility tags) still fail the session. They are outside #816's
     frame-cap charter and are reported as a follow-up candidate.
4. **Covered elsewhere:** none.

### Threat model

- **Boundary:** ffprobe output derived from untrusted file metadata, entering the worker
  result (widened: its size is now checked). There is no new boundary.
- **Actor:** whoever can place a file in a scanned library.
- **Control:** a bound on the compact snapshot size at the producer, before framing. The
  existing reader cap remains the receiver control. The failure message reveals only byte
  counts.
- **Out of scope:** ffprobe CPU or time on hostile files (the existing 30 s timeout).

## Success

1. A `probe_file` result for a 64-stream, about 80 KB ffprobe output is read through
   the default-cap HTTP client as a `Result` frame. Its normalized streams are intact and
   `raw` carries only the omitted-bytes marker.
2. A normalized snapshot above the bound produces a terminal `MalformedMedia` error frame,
   not an oversized frame.
3. In a scan session where one of two primaries' probes refuses with `MalformedMedia`, the
   session completes. That file is observed evidence-less, and the other publishes with
   evidence.
4. The ffprobe worker handler, in a release build and over HTTP, returns results on the two
   oversized corpus files that are under
   `MAX_FRAME_BYTES`, with `raw` omitted and every stream present.
5. `PROTOCOL_VERSION` stays 6.

## Validation

Focused tests prove Success 1–3 and the headroom pins
([plan](../plans/2026-10-30-probe-frame-cap.md)). Success 4 is an out-of-tree corpus check
that runs under the corpus lock and leaves the corpus read-only. Guardrail: `just ci`.
