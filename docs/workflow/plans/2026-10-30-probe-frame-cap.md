# Bounded probe snapshot (#816) implementation plan

Goal: implement [the spec](../specs/2026-10-30-probe-frame-cap-design.md) and
[ADR 0122](../../adr/0122-bounded-probe-snapshot.md). The probe worker keeps its snapshot
within `MAX_PROBE_SNAPSHOT_BYTES`, so no `probe_file` result exceeds the frame cap.

Architecture: two exported `voom-worker-protocol` constants with headroom pins; a
`bound_snapshot` step ending `normalize_ffprobe_json`; a new `WorkerError::MalformedMedia`
variant; no node-agent production change. Tech stack: Rust 2024, serde_json, tokio tests.

Expected implementation size: 220–320 changed lines (M) — file map: ~60 production lines, 160–260 test lines

## Global Constraints

- Toolchain from `rust-toolchain.toml`. Workspace lints apply: clippy pedantic, and
  `unwrap_used`/`panic` are denied outside test modules that already allow them.
- Unit tests in sibling `*_test.rs` files (ADR 0004). No new dependency. `PROTOCOL_VERSION`
  stays 6; no serialized struct changes shape.
- `crates/voom-ffprobe-worker/src/handler.rs` keeps these texts byte-identical and unique:
  `use crate::{WorkerError, normalize_ffprobe_json, observe_file_facts};`, both
  `Box::pin(observe_file_facts(&path))` blocks, and `fn verify_expected_facts(`.
  `just media-evidence-candidates-selftest` checks this.
- Builds and tests run under the shared corpus lock (`flock -s`); guardrail before push: `just ci`.

## File map

| File | Change |
|---|---|
| `crates/voom-worker-protocol/src/wire/ndjson.rs` | `DEFAULT_MAX_FRAME_BYTES` becomes `pub const MAX_FRAME_BYTES` |
| `crates/voom-worker-protocol/src/operations/probe_file.rs` (+`_test.rs`) | `MAX_PROBE_SNAPSHOT_BYTES`; two pin tests |
| `crates/voom-worker-protocol/src/lib.rs` | export both constants |
| `crates/voom-ffprobe-worker/src/normalize.rs` (+`_test.rs`) | `MalformedMedia` variant, `bound_snapshot`, unit tests |
| `crates/voom-ffprobe-worker/src/handler.rs` | one `From` arm |
| `crates/voom-ffprobe-worker/tests/probe_worker.rs` | HTTP success and refusal tests |
| `crates/voom-node-agent/src/scan_session.rs` (+`_test.rs`) | use the voom-core evidence bounds; mixed-session test |

## Task 1 — Protocol constants and headroom pins

Verification:
- Contract: a result frame whose snapshot is at the bound fits `MAX_FRAME_BYTES`
  (`result_frame_with_snapshot_at_bound_fits_one_frame`), and so does the matching scan
  evidence within `MAX_EVIDENCE_BYTES`
  (`scan_evidence_with_snapshot_at_bound_fits_the_evidence_budget`). Mode: focused-test in
  `probe_file_test.rs`. Red: compile failure until the constants exist. Green:
  `cargo test -p voom-worker-protocol snapshot_at_bound` (2 passed).

Steps:
1. Append to `probe_file_test.rs`:

```rust
const WORST_TIMESTAMP: &str = "2026-10-30T23:59:59.999999999Z";

fn snapshot_at_bound() -> serde_json::Value {
    let empty = serde_json::to_vec(&serde_json::json!({"pad": ""})).unwrap().len();
    let snapshot = serde_json::json!({"pad": "x".repeat(MAX_PROBE_SNAPSHOT_BYTES - empty)});
    assert_eq!(serde_json::to_vec(&snapshot).unwrap().len(), MAX_PROBE_SNAPSHOT_BYTES);
    snapshot
}

fn worst_facts() -> ObservedFileFacts {
    // The ffprobe worker never sets `local_file_key`.
    ObservedFileFacts {
        size_bytes: u64::MAX,
        content_hash: format!("blake3:{}", "f".repeat(64)),
        modified_at: Some(WORST_TIMESTAMP.to_owned()),
        local_file_key: None,
    }
}

#[test]
fn result_frame_with_snapshot_at_bound_fits_one_frame() {
    let result = ProbeFileResult {
        status: ProbeFileStatus::Probed,
        provider: "ffprobe".to_owned(),
        provider_version: "v".repeat(64),
        pre_probe: worst_facts(),
        post_probe: worst_facts(),
        snapshot: snapshot_at_bound(),
        hash_source: Some(HashSource::Reused),
    };
    let frame = crate::ProgressFrame::Result {
        lease_id: voom_core::LeaseId(u64::MAX),
        seq: u64::MAX,
        emitted_at: time::OffsetDateTime::parse(
            WORST_TIMESTAMP,
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap(),
        payload: serde_json::to_value(&result).unwrap(),
    };
    let bytes = serde_json::to_vec(&frame).unwrap().len();
    assert!(bytes <= crate::MAX_FRAME_BYTES, "{bytes} > {}", crate::MAX_FRAME_BYTES);
}

#[test]
fn scan_evidence_with_snapshot_at_bound_fits_the_evidence_budget() {
    let evidence = voom_core::ScanObservationEvidence {
        content_hash: format!("blake3:{}", "f".repeat(64)),
        size_bytes: u64::MAX,
        modified_at: WORST_TIMESTAMP.to_owned(),
        file_key: Some(voom_core::FileKeyFacts { dev: u64::MAX, ino: u64::MAX, nlink: u64::MAX }),
        sidecars: Vec::new(),
        probe_snapshot: snapshot_at_bound(),
        hash_source: Some(HashSource::Reused),
    };
    assert!(evidence.validate().is_ok(), "{:?}", evidence.validate());
}
```

2. Run the green command and expect a compile failure.
3. In `wire/ndjson.rs`, replace `const DEFAULT_MAX_FRAME_BYTES: usize = 64 * 1024;` with:

```rust
/// Largest NDJSON frame payload a reader accepts; every worker result must fit one frame.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
```

   Rename the one use in `NdjsonReader::new`.
4. In `operations/probe_file.rs`, add above `ProbeFileResult`:

```rust
/// Producer bound on `ProbeFileResult.snapshot` as compact JSON (ADR 0122). The 4 KiB below
/// `MAX_FRAME_BYTES` and `voom_core::MAX_EVIDENCE_BYTES` holds the result envelope and the
/// scan evidence fields; `probe_file_test.rs` pins both.
pub const MAX_PROBE_SNAPSHOT_BYTES: usize = 60 * 1024;
```

5. In `lib.rs`, add `MAX_PROBE_SNAPSHOT_BYTES` to the `operations::probe_file` export and
   change the ndjson export to `{MAX_FRAME_BYTES, NdjsonOutcome, NdjsonReader, NdjsonWriter}`.
6. Expect `2 passed` and a green `cargo test -p voom-worker-protocol`. Commit with
   `feat(worker-protocol): export the frame cap and probe snapshot bound`.

## Task 2 — Bounded snapshot in the ffprobe worker

Verification:
- Contract: below the bound, the snapshot keeps `raw.ffprobe_json`; above it, `raw` becomes
  the omitted-bytes marker, and the normalized streams stay whole.
  Mode: focused-test, `normalize_test.rs::{raw_kept_at_exact_bound, raw_omitted_one_byte_over_bound,
  dense_streams_keep_normalized_facts_without_raw}`.
- Contract: a normalized snapshot over the bound is refused as `MalformedMedia`.
  Mode: focused-test, `normalize_test.rs::oversized_normalized_snapshot_is_malformed_media`.
- Contract: over HTTP, through the default-cap reader, a dense probe returns a `Result` frame and
  an oversized one returns a `MalformedMedia` error frame.
  Mode: focused-test, `tests/probe_worker.rs::{dense_ffprobe_output_returns_bounded_snapshot_over_http,
  oversized_normalized_snapshot_returns_malformed_media_over_http}`. Red: before step 4, the
  dense test fails with `frame too large`, and the refusal test sees the same reader error.
- Green: `cargo test -p voom-ffprobe-worker` (all pass).

Steps:
1. Append to `normalize_test.rs`:

```rust
fn padded_probe(pad: usize) -> Value {
    serde_json::json!({"format": {"format_name": "matroska,webm", "x_pad": "p".repeat(pad)}})
}

fn snapshot_len(snapshot: &Value) -> usize {
    serde_json::to_vec(snapshot).unwrap().len()
}

/// The pad length that makes the full snapshot exactly `MAX_PROBE_SNAPSHOT_BYTES`.
fn exact_pad() -> usize {
    let base = normalize_ffprobe_json(padded_probe(0), "8.1.3", "2026-10-30T00:00:00Z").unwrap();
    voom_worker_protocol::MAX_PROBE_SNAPSHOT_BYTES - snapshot_len(&base)
}

#[test]
fn raw_kept_at_exact_bound() {
    let raw = padded_probe(exact_pad());
    let snapshot = normalize_ffprobe_json(raw.clone(), "8.1.3", "2026-10-30T00:00:00Z").unwrap();
    assert_eq!(snapshot_len(&snapshot), voom_worker_protocol::MAX_PROBE_SNAPSHOT_BYTES);
    assert_eq!(snapshot["raw"]["ffprobe_json"], raw);
}

#[test]
fn raw_omitted_one_byte_over_bound() {
    let raw = padded_probe(exact_pad() + 1);
    let raw_len = snapshot_len(&raw);
    let snapshot = normalize_ffprobe_json(raw, "8.1.3", "2026-10-30T00:00:00Z").unwrap();
    assert_eq!(snapshot["raw"], serde_json::json!({"ffprobe_json_omitted_bytes": raw_len}));
    assert_eq!(snapshot["container"]["format_name"], "matroska,webm");
}

#[test]
fn dense_streams_keep_normalized_facts_without_raw() {
    let streams: Vec<Value> = (0..64)
        .map(|index| serde_json::json!({"index": index, "codec_type": "subtitle",
            "codec_name": "subrip", "tags": {"language": "eng", "title": format!("t{index}"),
            "_STATISTICS_WRITING_APP": "w".repeat(1200)}}))
        .collect();
    let raw = serde_json::json!({"format": {"format_name": "matroska,webm"}, "streams": streams});
    let snapshot = normalize_ffprobe_json(raw, "8.1.3", "2026-10-30T00:00:00Z").unwrap();
    assert!(snapshot_len(&snapshot) <= voom_worker_protocol::MAX_PROBE_SNAPSHOT_BYTES);
    assert!(snapshot["raw"].get("ffprobe_json").is_none());
    let normalized = snapshot["streams"].as_array().unwrap();
    assert_eq!(normalized.len(), 64);
    assert_eq!(normalized[63]["title"], "t63");
    assert_eq!(normalized[63]["language"], "eng");
}

#[test]
fn oversized_normalized_snapshot_is_malformed_media() {
    let title = "t".repeat(voom_worker_protocol::MAX_PROBE_SNAPSHOT_BYTES);
    let raw = serde_json::json!({"streams": [{"index": 0, "codec_type": "subtitle",
        "tags": {"title": title}}]});
    let error = normalize_ffprobe_json(raw, "8.1.3", "2026-10-30T00:00:00Z").unwrap_err();
    assert_eq!(error.failure_class(), FailureClass::MalformedMedia);
}
```

2. Append to `tests/probe_worker.rs`. Each test writes the JSON to `dir/probe.json` and uses
   `write_fake_ffprobe(dir.path(), &format!("cat \"{}\"\nexit 0\n", json.display()))`,
   `write_wav`, `configured_ffprobe` and `dispatch_terminal_frame` as the existing tests do.
   - `dense_ffprobe_output_returns_bounded_snapshot_over_http`: the 64-stream JSON from step 1
     (more than 64 KiB raw). Assert that the terminal frame is `ProgressFrame::Result`, that its
     serialized length is ≤ `MAX_FRAME_BYTES`, that `payload["snapshot"]["streams"]` has 64
     entries, and that `payload["snapshot"]["raw"]["ffprobe_json_omitted_bytes"]` is a number.
   - `oversized_normalized_snapshot_returns_malformed_media_over_http`: the one-stream
     oversized-title JSON. Assert
     `assert_terminal_error(terminal, FailureClass::MalformedMedia, ErrorCode::MalformedMedia)`.
3. Run `cargo test -p voom-ffprobe-worker` and expect the new tests to fail, as listed above.
4. In `normalize.rs`, add the variant
   `#[error("malformed media: {0}")] MalformedMedia(String),` with the `failure_class` arm
   `Self::MalformedMedia(_) => FailureClass::MalformedMedia,`. Replace the three lines that
   build and insert `raw_object` with `bound_snapshot(snapshot, raw)`, and add:

```rust
/// Keeps the snapshot within `MAX_PROBE_SNAPSHOT_BYTES` (ADR 0122). The raw ffprobe JSON
/// goes first; a snapshot still over the bound is refused, never truncated.
fn bound_snapshot(mut snapshot: Map<String, Value>, raw: Value) -> Result<Value, WorkerError> {
    let raw_bytes = compact_len(&raw)?;
    snapshot.insert("raw".to_owned(), serde_json::json!({"ffprobe_json": raw}));
    if compact_len(&snapshot)? <= MAX_PROBE_SNAPSHOT_BYTES {
        return Ok(Value::Object(snapshot));
    }
    snapshot.insert(
        "raw".to_owned(),
        serde_json::json!({"ffprobe_json_omitted_bytes": raw_bytes}),
    );
    let bytes = compact_len(&snapshot)?;
    if bytes > MAX_PROBE_SNAPSHOT_BYTES {
        return Err(WorkerError::MalformedMedia(format!(
            "probe snapshot is {bytes} bytes without the raw ffprobe JSON, above the \
             {MAX_PROBE_SNAPSHOT_BYTES}-byte bound"
        )));
    }
    Ok(Value::Object(snapshot))
}

fn compact_len(value: &impl serde::Serialize) -> Result<usize, WorkerError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| malformed(format!("snapshot encode: {error}")))
}
```

   Import `voom_worker_protocol::MAX_PROBE_SNAPSHOT_BYTES`.
5. In `handler.rs`, add this arm to `impl From<WorkerError> for FfprobeError`:

```rust
            WorkerError::MalformedMedia(message) => Self::MalformedMedia {
                payload: serde_json::json!({
                    "stage": "normalize_snapshot",
                    "message": message,
                }),
                message,
            },
```

6. Expect `cargo test -p voom-ffprobe-worker` and `just media-evidence-candidates-selftest` to
   pass. Commit with `fix(ffprobe-worker): bound the probe snapshot below the frame cap`.

## Task 3 — One refused file does not fail the session

Verification:
- Contract: a `MalformedMedia` probe refusal for one primary leaves the session `succeeded`;
  that file is observed evidence-less and the other keeps its evidence.
  Mode: focused-test, `scan_session_test.rs::oversized_probe_refusal_degrades_only_that_file`.
  Red: before step 2 the new behavior variant does not exist (compile failure). After the
  test-support change it passes with no production change, which shows the existing
  `classify_single_file_error` mapping is the contract under test. Breaking that arm
  (map `MalformedMedia` to fatal) must turn it red. Green:
  `cargo test -p voom-node-agent oversized_probe_refusal` (1 passed).

Steps:
1. Add the test. Its candidates are `candidate("plain.mkv")` and `candidate("dense.mkv")`, then
   `ScanFrame::Result(2, 0)`. The hash behavior is `Agree`, and the probe behavior is
   `SingleFileBehavior::RefuseDense`. After `pump_scan_session`, assert the following:
   `LeaseOutcome::Complete`, `observed_count == 2`, `failed_content_count == 1`, and
   `session_status == "succeeded"`. Also assert that
   `SELECT provider_relative_locator, evidence_json IS NOT NULL FROM scan_observations
   WHERE scan_session_id = ? ORDER BY provider_relative_locator` returns
   `[("dense.mkv", false), ("plain.mkv", true)]`.
2. Add `RefuseDense` to `SingleFileBehavior`, documented as "Probe-only: refuses `dense.mkv`
   as oversized media and agrees on every other path". In `ScriptedSingleFileWorker::dispatch`,
   compute
   `let dense = request.payload["path"].as_str().is_some_and(|path| path.ends_with("dense.mkv"));`
   and add these match arms before the catch-all:
   `(OperationKind::ProbeFile, SingleFileBehavior::RefuseDense) if dense =>` an `Error` frame
   with `FailureClass::MalformedMedia`, `ErrorCode::MalformedMedia` and message
   `"scripted oversized snapshot"`; and
   `(OperationKind::ProbeFile, SingleFileBehavior::RefuseDense) =>` the same `Result` frame as
   the `Agree` arm.
3. Expect `1 passed`.
4. In `scan_session.rs`, delete the private `MAX_SIDECAR_ENTRIES` and `MAX_EVIDENCE_BYTES`
   consts and import `voom_core::{MAX_EVIDENCE_BYTES, MAX_EVIDENCE_SIDECARS}`. Rename the
   uses of `MAX_SIDECAR_ENTRIES`; the values are equal (64, 64 KiB). Run
   `cargo test -p voom-node-agent scan_session` and expect it to pass.
5. Commit with `test(node-agent): pin per-file degradation of a refused probe`.

## Task 4 — Corpus check and full guardrails

Verification: task-test-not-applicable — the real-corpus check reads private local media CI
cannot reach. Out of tree and under `flock -x` on the corpus lock, build the ffprobe handler
in release mode and dispatch it over the HTTP client (default-cap reader) on both oversized
files. Assert the frame is ≤ `MAX_FRAME_BYTES`, `raw` is the marker, and the stream count
equals ffprobe's own count. Use the guarded path: it skips about 140 GB of hash reads, and
it calls the same `normalize_ffprobe_json` as the unguarded path (`guard.rs:100`,
`handler.rs:74`). The fake-ffprobe HTTP test covers the unguarded path. Record the results in
the PR body and leave the corpus unmodified. Then run `just ci` bare, which must exit 0, and
record its duration.
