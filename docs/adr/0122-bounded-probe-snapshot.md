# 0122 — The probe worker bounds its snapshot below the frame cap

## Status

Accepted (2026-10-30). Implemented by #816.

## Context

Every worker result travels as one NDJSON frame, and every reader rejects a frame above
64 KiB (`wire/ndjson.rs`). The `probe_file` result carries the normalized snapshot, which
embeds the whole ffprobe JSON under `raw.ffprobe_json`. That JSON grows with stream count and
tag text. On the local real-media corpus two of 34 files (58 and 64 streams) produced results
of about 73 and 80 KB. The scan pump treats the reader error as a worker crash, so the scan
session failed and published nothing (#816). Output probes and every other `probe_file`
reader share the same cap and the same failure.

The snapshot also becomes scan evidence, which ADR 0077 bounds at 64 KiB per observation:
larger evidence degrades the observation to evidence-less, with no published identity. ADR
0077's budget is unchanged here.

Policy and planning read only the normalized fields. The Sprint 10 design keeps
`raw.ffprobe_json` "for operator diagnostics" and forbids planning from reading it.
`rg -n 'ffprobe_json' crates/*/src` at f8be728c finds no reader outside the ffprobe worker.

## Decision

1. `voom-worker-protocol` exports the frame cap as `MAX_FRAME_BYTES` (64 KiB, unchanged) and
   the snapshot bound as `MAX_PROBE_SNAPSHOT_BYTES` (60 KiB). Tests pin that a result whose
   snapshot sits at the bound still fits one frame, and that the matching scan evidence still
   fits `MAX_EVIDENCE_BYTES`.
2. The ffprobe normalizer measures the compact JSON of the snapshot. Above the bound it
   replaces `raw` with `{"ffprobe_json_omitted_bytes": <n>}`, where `n` is the size of the
   dropped JSON. The normalized fields stay whole.
3. If the snapshot is still above the bound, the probe fails with `MalformedMedia`, which the
   failure taxonomy classes non-retriable (`failure.rs`). The scan pump records the file as
   an evidence-less observation and the session goes on.
4. The worker protocol version stays 6. No message shape changes. The snapshot is opaque
   passthrough JSON (`media_snapshots.payload`), and its format marker stays `sprint10-v1`.

## Consequences

- Without raw, both failing corpus files have snapshots of 12.9 and 14.9 KB, and they scan
  with full identity evidence. A file whose normalized facts alone exceed 60 KiB (hundreds of
  streams, or a huge title tag) is recorded without identity rather than failing its session.
- `MalformedMedia` here means "beyond the size VOOM accepts", not "corrupt bytes". The
  message names the byte count and the bound.
- Changing the bound means editing one constant. Its pinning tests fail if the change would
  break the frame cap or `voom_core::MAX_EVIDENCE_BYTES`, which the scan pump now uses in
  place of its private copy.
- `raw.ffprobe_json` is lost only for oversized files, and the marker records how large it
  was. Container tags (title, year) appear only in raw, so these files do not persist them.
  The CLI test-support readers of raw (`tests/support/observed_state.rs`,
  `tests/support/structural_matrix.rs`) report such a file as missing raw evidence.

## Considered & rejected

- **Raise the frame cap for probe results.** verified: `scan_session.rs:917-918` (f8be728c)
  degrades evidence above `MAX_EVIDENCE_BYTES` (64 KiB, ADR 0077, unchanged), so a 73–80 KB
  snapshot would be scanned with no identity. Output probes would still need their own cap.
- **Have the pump degrade a too-large frame per file.** judgment: it hides a worker's protocol
  violation as a content failure, and the file still loses its identity.
- **Never embed `raw.ffprobe_json`.** verified: `rg -n 'ffprobe_json' crates` (f8be728c) shows
  CLI integration support reading `raw.ffprobe_json` tags from published snapshots
  (`tests/support/observed_state.rs`, `chaos_librarian_e2e.rs`). Changing every snapshot also
  goes beyond the defect.
- **Trim `raw` piecewise (drop stream tags first).** judgment: a partial raw copy is no longer
  provider output, and diagnostics would show a shape ffprobe never produced.
- **Have the frame writer refuse an over-cap frame.** judgment: `NdjsonWriter::emit`
  (`wire/ndjson.rs`) returning an error leaves the stream with no terminal frame, and the pump
  still reads that as a worker crash. Every worker would need a size-aware error path, which
  this defect does not justify, because no other operation result embeds tool JSON (spec,
  Sibling sweep).
- **Do nothing.** verified: #816's G3 run, where every whole-root scan of the corpus fails.
