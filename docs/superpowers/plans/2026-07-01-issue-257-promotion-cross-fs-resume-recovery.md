# Cross-filesystem promotion resume recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make cross-filesystem terminal-artifact promotion idempotently resumable so a copy-succeeds/remove-fails (or crash-before-repoint) never wedges a workflow permanently.

**Architecture:** Rework `move_terminal_artifact` in `crates/voom-control-plane/src/workflow/coordinator/promotion.rs` to (1) copy through a hidden temp sibling then atomic same-FS rename so `dest` is never partial, (2) recognise a byte-identical resumed copy in the collision branch and repoint instead of erroring, and (3) treat source removal as best-effort cleanup. The caller's DB-repoint transaction is unchanged.

**Tech Stack:** Rust, tokio (`fs`, `io-util`), sqlx; sibling unit tests (`*_test.rs` via `#[path]`, ADR 0004); `tracing` for logs.

## Global Constraints

- Spec: `docs/specs/promotion-cross-fs-resume-recovery-257.md`. Every task's requirements implicitly include it.
- Style: `[workspace.lints]` pedantic on; `unwrap`/`expect`/`panic` denied in non-test code. Functions ≤100 lines, ≤8 cyclomatic complexity, ≤5 positional params, 100-char lines, absolute imports only.
- Error type: return `voom_core::VoomError`; use `VoomError::Config` for filesystem errors (matches existing code at `promotion.rs:132-143`), `VoomError::Internal` for a missing-file-name invariant break.
- Preserve the public error string `"promotion destination already exists"` verbatim — it is asserted by `crates/voom-control-plane/src/workflow/coordinator/mod_test.rs:824`, `crates/voom-control-plane/tests/audio_transcode_flow.rs:172`, and CLI snapshot `crates/voom-cli/tests/snapshots/compliance_envelope__execute_scanned_remux_existing_target_outputs_failure_envelope.snap`.
- Single-writer promotion (ADR 0001/0009): no cross-process interlock needed.
- Guardrails before each commit: `just fmt`, `cargo test -p voom-control-plane <focused>`. Before push: full `just ci`.
- Testing: sibling `promotion_test.rs`; behavior + edges, not implementation (AGENTS.md Rule 9). No inline `#[cfg(test)] mod tests` in `src/`.

---

## File Structure

- Modify: `crates/voom-control-plane/src/workflow/coordinator/promotion.rs` — rewrite `move_terminal_artifact` and add four private free helpers: `resolve_existing_destination`, `copy_into_place`, `promotion_temp_path`, `remove_promoted_source`, `files_have_equal_contents`. Add the `#[path]` sibling-test link at the bottom.
- Create: `crates/voom-control-plane/src/workflow/coordinator/promotion_test.rs` — sibling unit tests.

---

### Task 1: Byte-equality helper `files_have_equal_contents`

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/coordinator/promotion.rs` (add helper + sibling-test link)
- Create: `crates/voom-control-plane/src/workflow/coordinator/promotion_test.rs`

**Interfaces:**
- Produces: `async fn files_have_equal_contents(a: &Path, b: &Path) -> Result<bool, VoomError>` — `Ok(true)` iff both files hold identical bytes; size-first then chunked streaming compare; a stat/open/read failure returns `Err`.

- [ ] **Step 1: Add the `#[path]` sibling-test link at the bottom of `promotion.rs`**

```rust
#[cfg(test)]
#[path = "promotion_test.rs"]
mod tests;
```

- [ ] **Step 2: Write the failing tests in `promotion_test.rs`**

```rust
use super::*;

async fn write(path: &Path, bytes: &[u8]) {
    tokio::fs::write(path, bytes).await.unwrap();
}

#[tokio::test]
async fn equal_contents_true_for_identical_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    write(&a, b"terminal-bytes").await;
    write(&b, b"terminal-bytes").await;
    assert!(files_have_equal_contents(&a, &b).await.unwrap());
}

#[tokio::test]
async fn equal_contents_false_for_same_size_different_bytes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    write(&a, b"aaaa").await;
    write(&b, b"bbbb").await;
    assert!(!files_have_equal_contents(&a, &b).await.unwrap());
}

#[tokio::test]
async fn equal_contents_false_for_different_size() {
    let tmp = tempfile::TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    write(&a, b"short").await;
    write(&b, b"longer-content").await;
    assert!(!files_have_equal_contents(&a, &b).await.unwrap());
}

#[tokio::test]
async fn equal_contents_true_for_empty_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    write(&a, b"").await;
    write(&b, b"").await;
    assert!(files_have_equal_contents(&a, &b).await.unwrap());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p voom-control-plane equal_contents -- --nocapture`
Expected: FAIL to compile — `files_have_equal_contents` not found.

- [ ] **Step 4: Implement the helper in `promotion.rs`**

Add `use tokio::io::AsyncReadExt;` to the imports, then:

```rust
/// Whether two files hold identical bytes. Size-first (a cheap reject), then a
/// chunked streaming compare so a multi-GB media artifact is never loaded whole.
async fn files_have_equal_contents(a: &Path, b: &Path) -> Result<bool, VoomError> {
    let len_a = tokio::fs::metadata(a)
        .await
        .map_err(|err| VoomError::Config(format!("stat {} to compare: {err}", a.display())))?
        .len();
    let len_b = tokio::fs::metadata(b)
        .await
        .map_err(|err| VoomError::Config(format!("stat {} to compare: {err}", b.display())))?
        .len();
    if len_a != len_b {
        return Ok(false);
    }
    let mut file_a = tokio::fs::File::open(a)
        .await
        .map_err(|err| VoomError::Config(format!("open {} to compare: {err}", a.display())))?;
    let mut file_b = tokio::fs::File::open(b)
        .await
        .map_err(|err| VoomError::Config(format!("open {} to compare: {err}", b.display())))?;
    let mut buf_a = vec![0u8; 64 * 1024];
    let mut buf_b = vec![0u8; 64 * 1024];
    let mut remaining = len_a;
    while remaining > 0 {
        let chunk = remaining.min(buf_a.len() as u64) as usize;
        file_a
            .read_exact(&mut buf_a[..chunk])
            .await
            .map_err(|err| VoomError::Config(format!("read {} to compare: {err}", a.display())))?;
        file_b
            .read_exact(&mut buf_b[..chunk])
            .await
            .map_err(|err| VoomError::Config(format!("read {} to compare: {err}", b.display())))?;
        if buf_a[..chunk] != buf_b[..chunk] {
            return Ok(false);
        }
        remaining -= chunk as u64;
    }
    Ok(true)
}
```

- [ ] **Step 5: Run tests to verify they pass, then fmt + clippy**

Run: `cargo test -p voom-control-plane equal_contents`
Expected: PASS (4 tests).
Run: `just fmt && cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-control-plane/src/workflow/coordinator/promotion.rs \
        crates/voom-control-plane/src/workflow/coordinator/promotion_test.rs
git commit -m "feat(control-plane): add byte-equality helper for promotion recovery

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Atomic copy-into-place fallback + best-effort source removal

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/coordinator/promotion.rs`
- Modify: `crates/voom-control-plane/src/workflow/coordinator/promotion_test.rs`

**Interfaces:**
- Consumes: `files_have_equal_contents` (Task 1).
- Produces:
  - `fn promotion_temp_path(dest: &Path) -> Result<PathBuf, VoomError>` — hidden dotfile sibling `.voom-promote.<file_name>.partial` in `dest`'s dir; `Err(Internal)` if `dest` has no file name.
  - `async fn remove_promoted_source(current: &Path)` — best-effort `remove_file`; logs `tracing::warn!` on failure, never errors.
  - `async fn copy_into_place(current: &Path, dest: &Path) -> Result<(), VoomError>` — copy `current` into the temp sibling, atomically `rename` it to `dest`, then `remove_promoted_source(current)`. Removes the temp best-effort on failure.

- [ ] **Step 1: Write the failing test in `promotion_test.rs`**

```rust
#[tokio::test]
async fn copy_into_place_moves_bytes_and_cleans_up() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("work").join("Movie.hevc.mkv");
    let dest = tmp.path().join("out").join("Movie.hevc.mkv");
    tokio::fs::create_dir_all(current.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::create_dir_all(dest.parent().unwrap())
        .await
        .unwrap();
    write(&current, b"terminal-bytes").await;

    copy_into_place(&current, &dest).await.unwrap();

    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
    assert!(tokio::fs::symlink_metadata(&current).await.is_err());
    let temp = dest.with_file_name(".voom-promote.Movie.hevc.mkv.partial");
    assert!(tokio::fs::symlink_metadata(&temp).await.is_err());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-control-plane copy_into_place_moves_bytes -- --nocapture`
Expected: FAIL to compile — `copy_into_place` not found.

- [ ] **Step 3: Implement the three helpers in `promotion.rs`**

Add `use std::ffi::OsString;` to imports, then:

```rust
/// Hidden temp sibling for the copy fallback. A dotfile prefixed/suffixed out of
/// the plain-filename destination namespace, so it can never equal another
/// artifact's promoted destination in a shared output dir.
fn promotion_temp_path(dest: &Path) -> Result<PathBuf, VoomError> {
    let file_name = dest.file_name().ok_or_else(|| {
        VoomError::Internal(format!(
            "promotion destination has no file name: {}",
            dest.display()
        ))
    })?;
    let mut temp_name = OsString::from(".voom-promote.");
    temp_name.push(file_name);
    temp_name.push(".partial");
    Ok(dest.with_file_name(temp_name))
}

/// Remove a promoted artifact's source once its bytes are safe at the
/// destination. Best-effort: the promotion's commit is the durable location
/// repoint, so a failed cleanup is logged, not fatal, and cannot wedge a resume.
async fn remove_promoted_source(current: &Path) {
    if let Err(err) = tokio::fs::remove_file(current).await {
        tracing::warn!(
            source = %current.display(),
            error = %err,
            "promoted terminal artifact is placed at its destination but removing \
             the source failed; leaving an orphaned source in the working dir"
        );
    }
}

/// Place a terminal artifact at `dest` across filesystems without ever leaving a
/// partial `dest`: stream into a hidden temp sibling on `dest`'s filesystem, then
/// atomically `rename` it into place (an intra-filesystem rename is atomic). The
/// source is removed best-effort afterward. Used when a direct `rename` fails
/// (typically a cross-filesystem `EXDEV`).
async fn copy_into_place(current: &Path, dest: &Path) -> Result<(), VoomError> {
    let temp = promotion_temp_path(dest)?;
    tokio::fs::copy(current, &temp).await.map_err(|err| {
        VoomError::Config(format!(
            "copy terminal artifact {} -> {}: {err}",
            current.display(),
            temp.display()
        ))
    })?;
    if let Err(err) = tokio::fs::rename(&temp, dest).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(VoomError::Config(format!(
            "place terminal artifact {} -> {}: {err}",
            temp.display(),
            dest.display()
        )));
    }
    remove_promoted_source(current).await;
    Ok(())
}
```

- [ ] **Step 4: Run to verify it passes, then fmt + clippy**

Run: `cargo test -p voom-control-plane copy_into_place_moves_bytes`
Expected: PASS.
Run: `just fmt && cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-control-plane/src/workflow/coordinator/promotion.rs \
        crates/voom-control-plane/src/workflow/coordinator/promotion_test.rs
git commit -m "feat(control-plane): atomic copy-into-place for cross-fs promotion

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Wire recovery into `move_terminal_artifact`

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/coordinator/promotion.rs:103-146` (doc comment + function body)
- Modify: `crates/voom-control-plane/src/workflow/coordinator/promotion_test.rs`

**Interfaces:**
- Consumes: `files_have_equal_contents`, `copy_into_place` (Tasks 1–2).
- Produces:
  - `async fn resolve_existing_destination(current: &Path, dest: &Path, dest_meta: &std::fs::Metadata) -> Result<PathBuf, VoomError>` — already-moved (source gone) → `Ok(dest)`; source present + `dest` is a regular file byte-equal to `current` → recover (log, best-effort remove, `Ok(dest)`); otherwise the `"promotion destination already exists"` error.
  - Rewritten `move_terminal_artifact(current, dest) -> Result<PathBuf, VoomError>` — same signature; delegates a pre-existing `dest` to `resolve_existing_destination`, else `rename` or `copy_into_place`.

- [ ] **Step 1: Write the failing tests in `promotion_test.rs`**

```rust
#[tokio::test]
async fn resumed_copy_recovers_and_removes_source() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"terminal-bytes").await;
    write(&dest, b"terminal-bytes").await; // simulates copy-done, remove-failed

    let returned = move_terminal_artifact(&current, &dest).await.unwrap();

    assert_eq!(returned, dest);
    assert!(tokio::fs::symlink_metadata(&current).await.is_err());
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
}

#[tokio::test]
async fn genuine_collision_same_size_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"aaaaaaaaaaaaaa").await;
    write(&dest, b"bbbbbbbbbbbbbb").await;

    let err = move_terminal_artifact(&current, &dest).await.unwrap_err();

    assert!(
        err.to_string().contains("promotion destination already exists"),
        "unexpected: {err}"
    );
    assert!(tokio::fs::symlink_metadata(&current).await.is_ok());
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"bbbbbbbbbbbbbb");
}

#[tokio::test]
async fn genuine_collision_different_size_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"terminal-bytes").await;
    write(&dest, b"a-different-shorter").await;

    let err = move_terminal_artifact(&current, &dest).await.unwrap_err();

    assert!(err.to_string().contains("promotion destination already exists"));
    assert!(tokio::fs::symlink_metadata(&current).await.is_ok());
}

#[tokio::test]
async fn directory_destination_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"terminal-bytes").await;
    tokio::fs::create_dir(&dest).await.unwrap();

    let err = move_terminal_artifact(&current, &dest).await.unwrap_err();

    assert!(err.to_string().contains("promotion destination already exists"));
    assert!(tokio::fs::symlink_metadata(&current).await.is_ok());
}

#[tokio::test]
async fn already_moved_source_gone_repoints() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&dest, b"terminal-bytes").await; // current absent

    let returned = move_terminal_artifact(&current, &dest).await.unwrap();

    assert_eq!(returned, dest);
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
}

#[tokio::test]
async fn normal_move_dest_absent_places_and_removes_source() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"terminal-bytes").await;

    let returned = move_terminal_artifact(&current, &dest).await.unwrap();

    assert_eq!(returned, dest);
    assert!(tokio::fs::symlink_metadata(&current).await.is_err());
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p voom-control-plane -- resumed_copy_recovers genuine_collision directory_destination already_moved normal_move`
Expected: `resumed_copy_recovers_and_removes_source` FAILS (today returns the collision error); the others may pass on the current code. This test is the regression guard for the fix.

- [ ] **Step 3: Rewrite the function + doc comment in `promotion.rs`**

Replace the doc comment and body at `promotion.rs:103-146` with:

```rust
/// Move a terminal artifact into its promoted destination, add-only.
///
/// A live foreign destination collision fails the run (mirrors the commit's
/// no-replace contract). A destination that already holds this artifact's bytes
/// is a resume of an interrupted promotion — recognised and repointed rather than
/// failed: either the source is already gone (an earlier run promoted and crashed
/// before repointing) or the source is still present and byte-equal to the
/// destination (a cross-filesystem copy whose source removal or DB repoint did not
/// complete). Cross-filesystem placement goes through a temp sibling so the
/// destination is never observed partial.
async fn move_terminal_artifact(current: &Path, dest: &Path) -> Result<PathBuf, VoomError> {
    match tokio::fs::symlink_metadata(dest).await {
        Ok(dest_meta) => return resolve_existing_destination(current, dest, &dest_meta).await,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(VoomError::Config(format!(
                "stat promotion destination {}: {err}",
                dest.display()
            )));
        }
    }
    if tokio::fs::rename(current, dest).await.is_ok() {
        return Ok(dest.to_path_buf());
    }
    // A failed rename (typically a cross-filesystem EXDEV) falls back to an atomic
    // copy-into-place: stream into a temp sibling, then rename it over dest.
    copy_into_place(current, dest).await?;
    Ok(dest.to_path_buf())
}

/// Classify a pre-existing promotion destination: a resumed/interrupted promotion
/// of this artifact (repoint) versus a genuine foreign collision (fail).
async fn resolve_existing_destination(
    current: &Path,
    dest: &Path,
    dest_meta: &std::fs::Metadata,
) -> Result<PathBuf, VoomError> {
    if tokio::fs::symlink_metadata(current).await.is_err() {
        // Source gone: an earlier run promoted the bytes and crashed before the
        // repoint. Resume completes the repoint.
        return Ok(dest.to_path_buf());
    }
    if dest_meta.file_type().is_file() && files_have_equal_contents(current, dest).await? {
        tracing::info!(
            source = %current.display(),
            destination = %dest.display(),
            "recovered an interrupted cross-filesystem promotion; the source is \
             already copied to the destination"
        );
        remove_promoted_source(current).await;
        return Ok(dest.to_path_buf());
    }
    Err(VoomError::Config(format!(
        "promotion destination already exists: {}",
        dest.display()
    )))
}
```

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `cargo test -p voom-control-plane -- resumed_copy_recovers genuine_collision directory_destination already_moved normal_move`
Expected: PASS (all).

- [ ] **Step 5: Run the pre-existing collision regression tests**

Run: `cargo test -p voom-control-plane zero_phase_promotion_failure_preserves_seed_file_phases`
Expected: PASS (foreign `dest` = `b"existing"` still errors).

- [ ] **Step 6: fmt + clippy**

Run: `just fmt && cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-control-plane/src/workflow/coordinator/promotion.rs \
        crates/voom-control-plane/src/workflow/coordinator/promotion_test.rs
git commit -m "fix(control-plane): recover interrupted cross-fs promotion on resume

A cross-filesystem promotion copies then removes the source before the DB
location is repointed. If removal failed (or the process was killed) after the
copy, both source and destination existed and the collision check erased the
run permanently. Recognise a destination that byte-equals the still-present
source as a resumed promotion and repoint; copy through a temp sibling so the
destination is never partial; make source removal best-effort.

Closes #257

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Full-suite verification

**Files:** none (verification only).

- [ ] **Step 1: Run the full CI suite**

Run: `just ci`
Expected: green — `fmt-check`, `lint`, `check-test-layout` (sibling-test convention satisfied by the `#[path]` link), `test`, `doc`, `deny`, `audit`.

- [ ] **Step 2: If `check-test-layout` or any check fails, fix and re-run**

The most likely failure is a missing/mismatched `#[path = "promotion_test.rs"]` link — confirm it exists at the bottom of `promotion.rs`. Fix, re-run `just ci`.

---

## Self-Review

**Spec coverage:**
- D1 atomic copy via temp → Task 2 (`copy_into_place` + `promotion_temp_path`) and its direct helper test.
- D2 content-verified recovery → Task 3 (`resolve_existing_destination`) + `resumed_copy_recovers`, `genuine_collision_*`, `directory_destination`, `already_moved` tests; byte-equality → Task 1.
- D3 best-effort removal → Task 2 (`remove_promoted_source`), exercised in Tasks 2–3 tests.
- Temp namespace disjointness → `promotion_temp_path` dotfile scheme (Task 2) + cleanup assertion in `copy_into_place_moves_bytes_and_cleans_up`.
- Preserve `"promotion destination already exists"` → Task 3 Step 5 regression run.
- Acceptance criteria (all 7 bullets) → mapped to the Task 1–3 tests.

**Placeholder scan:** No TBD/TODO/"handle edge cases"; every code step shows full code.

**Type consistency:** `files_have_equal_contents(&Path,&Path)->Result<bool,VoomError>`, `copy_into_place(&Path,&Path)->Result<(),VoomError>`, `promotion_temp_path(&Path)->Result<PathBuf,VoomError>`, `remove_promoted_source(&Path)`, `resolve_existing_destination(&Path,&Path,&std::fs::Metadata)->Result<PathBuf,VoomError>`, `move_terminal_artifact(&Path,&Path)->Result<PathBuf,VoomError>` — used consistently across tasks. `.voom-promote.<file_name>.partial` temp name identical in helper and test.
