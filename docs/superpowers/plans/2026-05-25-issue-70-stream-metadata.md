# Issue 70 Stream Metadata Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve ffprobe stream language and default/forced disposition facts in durable media snapshot payloads.

**Architecture:** Keep the existing JSON snapshot persistence boundary. Extend `voom-ffprobe-worker` normalization to retain selected stream metadata, then remove the Chaos observed-state export fallback that inferred MP4 language values.

**Tech Stack:** Rust, serde_json, existing sibling unit test layout, `just` verification commands.

---

### Task 1: Add Normalizer Regression Tests

**Files:**
- Modify: `crates/voom-ffprobe-worker/src/normalize_test.rs`

- [x] Add `normalizes_stream_language_and_disposition_for_mp4` with raw ffprobe JSON containing an MP4 audio stream:

```rust
#[test]
fn normalizes_stream_language_and_disposition_for_mp4() {
    let raw = serde_json::json!({
        "format": { "format_name": "mov,mp4" },
        "streams": [
            {
                "index": 1,
                "codec_type": "audio",
                "codec_name": "aac",
                "tags": { "language": "und" },
                "disposition": { "default": 1, "forced": 0 }
            }
        ]
    });

    let snapshot = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z").unwrap();

    assert_eq!(snapshot["streams"][0]["language"], "und");
    assert_eq!(snapshot["streams"][0]["disposition"]["default"], true);
    assert_eq!(snapshot["streams"][0]["disposition"]["forced"], false);
}
```

- [x] Add `normalizes_stream_language_and_disposition_for_mkv_subtitles` with MKV audio and subtitle streams:

```rust
#[test]
fn normalizes_stream_language_and_disposition_for_mkv_subtitles() {
    let raw = serde_json::json!({
        "format": { "format_name": "matroska,webm" },
        "streams": [
            {
                "index": 0,
                "codec_type": "audio",
                "codec_name": "flac",
                "tags": { "language": "eng" },
                "disposition": { "default": true }
            },
            {
                "index": 1,
                "codec_type": "subtitle",
                "codec_name": "subrip",
                "tags": { "language": "spa" },
                "disposition": { "forced": "1" }
            }
        ]
    });

    let snapshot = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z").unwrap();

    assert_eq!(snapshot["streams"][0]["language"], "eng");
    assert_eq!(snapshot["streams"][0]["disposition"]["default"], true);
    assert!(snapshot["streams"][0]["disposition"].get("forced").is_none());
    assert_eq!(snapshot["streams"][1]["kind"], "subtitle");
    assert_eq!(snapshot["streams"][1]["language"], "spa");
    assert_eq!(snapshot["streams"][1]["disposition"]["forced"], true);
}
```

- [x] Add `rejects_malformed_disposition_values`:

```rust
#[test]
fn rejects_malformed_disposition_values() {
    let raw = serde_json::json!({
        "format": {},
        "streams": [
            {
                "index": 0,
                "codec_type": "audio",
                "disposition": { "default": "maybe" }
            }
        ]
    });

    let result = normalize_ffprobe_json(raw, "7.0", "2026-05-24T00:00:00Z");

    assert!(matches!(
        result.as_ref().map_err(WorkerError::failure_class),
        Err(voom_core::FailureClass::MalformedWorkerResult)
    ));
}
```

- [x] Run `cargo test -p voom-ffprobe-worker normalize -- --nocapture`.
- [x] Expected: the new tests fail because `language` and `disposition` are missing, and malformed disposition is not rejected yet.

### Task 2: Implement Stream Metadata Normalization

**Files:**
- Modify: `crates/voom-ffprobe-worker/src/normalize.rs`

- [x] In `stream_objects`, after the existing stream scalar fields, copy language and disposition:

```rust
insert_stream_language(input, &mut output);
insert_disposition(input, &mut output)?;
```

- [x] Add language helper below `stream_objects`:

```rust
fn insert_stream_language(input: &Map<String, Value>, output: &mut Map<String, Value>) {
    let Some(tags) = input.get("tags").and_then(Value::as_object) else {
        return;
    };
    insert_string_as(tags, output, "language", "language");
}
```

- [x] Add disposition helpers below `insert_stream_language`:

```rust
fn insert_disposition(
    input: &Map<String, Value>,
    output: &mut Map<String, Value>,
) -> Result<(), WorkerError> {
    let Some(disposition) = input.get("disposition") else {
        return Ok(());
    };
    let object = disposition
        .as_object()
        .ok_or_else(|| malformed("disposition must be a JSON object"))?;
    let mut normalized = Map::new();
    for key in ["default", "forced"] {
        if let Some(value) = object.get(key) {
            normalized.insert(key.to_owned(), Value::Bool(disposition_bool(value, key)?));
        }
    }
    if !normalized.is_empty() {
        output.insert("disposition".to_owned(), Value::Object(normalized));
    }
    Ok(())
}

fn disposition_bool(value: &Value, key: &str) -> Result<bool, WorkerError> {
    if let Some(value) = value.as_bool() {
        return Ok(value);
    }
    if let Some(value) = value.as_u64() {
        return match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(malformed(format!("{key} disposition must be 0 or 1"))),
        };
    }
    if let Some(value) = value.as_str() {
        return match value {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(malformed(format!("{key} disposition must be 0 or 1"))),
        };
    }
    Err(malformed(format!("{key} disposition must be 0 or 1")))
}
```

- [x] Run `cargo test -p voom-ffprobe-worker normalize -- --nocapture`.
- [x] Expected: all normalizer tests pass.

### Task 3: Remove Observed-State Language Inference

**Files:**
- Modify: `crates/voom-cli/tests/support/observed_state.rs`
- Modify: `crates/voom-cli/tests/chaos_librarian_e2e.rs`

- [x] Add focused tests in `chaos_librarian_e2e.rs`:

```rust
#[test]
fn observed_state_uses_stream_language_from_snapshot() {
    let stream = serde_json::json!({
        "kind": "audio",
        "codec_name": "aac",
        "language": "und"
    });

    let observed = support::observed_state::probed_stream_for_test(&stream).unwrap();

    assert_eq!(observed["language"], "und");
}

#[test]
fn observed_state_does_not_infer_mp4_language_when_snapshot_omits_it() {
    let stream = serde_json::json!({
        "kind": "audio",
        "codec_name": "aac"
    });

    let observed = support::observed_state::probed_stream_for_test(&stream).unwrap();

    assert!(observed.get("language").is_none());
}
```

- [x] Expose a test-only wrapper in `observed_state.rs`:

```rust
#[cfg(test)]
pub fn probed_stream_for_test(stream: &Value) -> Option<Value> {
    probed_stream(stream)
}
```

- [x] Remove `.or_else(|| mp4_default_language(container))` from `probed_stream`.
- [x] Delete `mp4_default_language`.
- [x] Run `cargo test -p voom-cli --test chaos_librarian_e2e observed_state -- --nocapture`.
- [x] Expected: the new tests fail before removing the fallback and pass after the fallback is removed.

### Task 4: Final Verification and Reviews

**Files:**
- Validate all changed files.

- [x] Run `cargo test -p voom-ffprobe-worker normalize`.
- [x] Run `cargo test -p voom-cli --test chaos_librarian_e2e observed_state`.
- [x] Run `just fmt-check`.
- [x] Run `just lint`.
- [x] Run `just test`.
- [x] Run adversarial code review and address material findings.
- [ ] Run simplification review and address the most relevant recommendations.
- [ ] Run `just ci`.
- [ ] Commit the branch.
