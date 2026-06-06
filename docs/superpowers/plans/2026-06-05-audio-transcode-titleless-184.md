# Audio-transcode title-less plannability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a `transcode audio` phase plan and commit against media whose audio streams have no `title` tag, by removing the per-stream preservation-fact gate.

**Architecture:** Per ADR-0011, the genuine plannability floor for an audio transcode is a known source codec + container, already enforced inside `transcode_audio_shape`. The `has_transcode_preservation_facts` predicate (requiring `language`+`title`+`channels`+`commentary` on every selected stream) conflates preservation completeness with plannability. Delete it and its two call sites; descriptive facts become pure preservation passthrough, already `None`-tolerant in `worker_contract.rs` (equality checks) and `commit.rs` (write-when-`Some`).

**Tech Stack:** Rust (workspace crates `voom-plan`, `voom-control-plane`), `cargo test`, sibling `*_test.rs` unit tests (ADR-0004), `just ci` guardrails.

**Pre-req already landed on this branch:** ADR-0011 and the closeout spec's resolved note are committed. No further doc edits needed.

**Commit strategy (read before starting):** `has_transcode_preservation_facts` is defined in `voom-plan` and imported by `voom-control-plane`. Deleting it from `voom-plan` breaks `voom-control-plane`'s compile until the importing crate is also updated. Therefore the gate removal is **one atomic commit across both crates** (Task 1) — splitting it would leave an intermediate commit where `cargo build --workspace` fails, violating "green at every commit." Tests for both crates are written first (they fail at runtime while the gate still exists), then the gate is removed from both crates, then a single commit. The intermediate steps between the two removals do not compile the workspace; that is expected and never committed.

---

### Task 1: Remove the audio-transcode preservation gate (both crates, one commit)

**Files:**
- Modify: `crates/voom-plan/src/planner/audio/selection.rs` (delete gate block in `transcode_audio_shape`; delete `has_transcode_preservation_facts`)
- Modify: `crates/voom-plan/src/planner/audio/mod.rs:14` (drop re-export)
- Modify: `crates/voom-plan/src/audio.rs:5` (drop re-export)
- Test: `crates/voom-plan/src/planner/audio/selection_test.rs`
- Modify: `crates/voom-control-plane/src/audio/selection.rs` (delete gate check + import)
- Test: `crates/voom-control-plane/src/audio/selection_test.rs`

- [ ] **Step 1: Rewrite the `voom-plan` unit tests (failing first)**

In `crates/voom-plan/src/planner/audio/selection_test.rs`:
- Remove `has_transcode_preservation_facts` from the `use super::{ … }` import (line ~6).
- Delete the test `transcode_preservation_facts_require_language_title_channels_and_commentary` (lines ~10-32).
- Replace the test `transcode_audio_shape_blocks_missing_preservation_facts` (lines ~104-114) with:

```rust
#[test]
fn transcode_audio_shape_plans_streams_missing_descriptive_facts() {
    // No per-stream descriptive fact is a transcode build input (ADR-0011);
    // a stream with a known codec plans regardless of title/commentary/
    // language/channels presence.
    let stream = SnapshotAudioStreamFact {
        title: None,
        language: None,
        channels: None,
        disposition: AudioDispositionFact {
            default: false,
            forced: false,
            commentary: None,
        },
        commentary: None,
        ..audio_fact(Some(false))
    };
    let snapshot = snapshot_with_audio_facts(vec![stream]);

    assert_eq!(
        transcode_audio_shape(&snapshot, "opus", AUDIO_TRANSCODE_CONTAINER, None),
        AudioPlanShape::Planned
    );
}

#[test]
fn transcode_audio_shape_blocks_stream_without_codec() {
    // Codec is the real plannability floor: without it the shape cannot decide
    // no-op vs transcode, so it blocks.
    let mut stream = audio_fact(Some(false));
    stream.codec = None;
    let snapshot = snapshot_with_audio_facts(vec![stream]);

    assert_eq!(
        transcode_audio_shape(&snapshot, "opus", AUDIO_TRANSCODE_CONTAINER, None),
        AudioPlanShape::Blocked(AudioPlanningBlock::InsufficientSnapshotFacts)
    );
}
```

- [ ] **Step 2: Rewrite the `voom-control-plane` gate test (failing first)**

In `crates/voom-control-plane/src/audio/selection_test.rs`, replace the test
`missing_selected_language_title_default_facts_block_transcode_preservation`
(lines ~147-165) with:

```rust
#[test]
fn transcode_selection_admits_streams_missing_descriptive_facts() {
    // No per-stream descriptive fact gates runtime selection (ADR-0011):
    // title-less, language-less, and commentary-less streams all select.
    for stream in [
        audio("a-1", 1, "aac", None, Some("Main"), Some(false)),
        audio("a-1", 1, "aac", Some("eng"), None, Some(false)),
        audio("a-1", 1, "aac", Some("eng"), Some("Main"), None),
    ] {
        let snapshot = snapshot_with_streams(vec![stream]);

        let selection = transcode_selection_from_payload_and_snapshot(
            &transcode_payload(&Value::Null),
            &snapshot,
        )
        .unwrap();

        assert_eq!(
            selection
                .selection
                .selected_streams
                .iter()
                .map(|stream| stream.snapshot_stream_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-1"]
        );
    }
}
```

- [ ] **Step 3: Run both crates' tests — verify the new "admit/plan" tests FAIL**

Run: `cargo test -p voom-plan transcode_audio_shape_ 2>&1 | tail -20`
Expected: `transcode_audio_shape_plans_streams_missing_descriptive_facts` FAILS
(asserts `Planned`, gets `Blocked(InsufficientSnapshotFacts)` from the live gate);
`transcode_audio_shape_blocks_stream_without_codec` PASSES (codec check precedes
the gate). Both crates still compile here — the function still exists.

Run: `cargo test -p voom-control-plane transcode_selection_admits 2>&1 | tail -20`
Expected: FAIL — `.unwrap()` panics on `Err("audio snapshot has insufficient stream facts")`.

- [ ] **Step 4: Remove the gate, function, and both re-exports in `voom-plan`**

In `crates/voom-plan/src/planner/audio/selection.rs`, delete the gate block in
`transcode_audio_shape` (currently lines ~221-226):

```rust
    if selected
        .iter()
        .any(|stream| !has_transcode_preservation_facts(stream))
    {
        return AudioPlanShape::Blocked(AudioPlanningBlock::InsufficientSnapshotFacts);
    }
```

Delete the function and its doc comment (currently lines ~291-301):

```rust
/// Returns whether a selected audio stream carries the facts required to
/// preserve its metadata across a transcode (language, title, channels, and a
/// known commentary disposition). Audio transcode planning and the
/// control-plane runtime selection share this rule.
#[must_use]
pub fn has_transcode_preservation_facts(stream: &SnapshotAudioStreamFact) -> bool {
    stream.language.is_some()
        && stream.title.is_some()
        && stream.channels.is_some()
        && stream.disposition.commentary.is_some()
}
```

Remove `has_transcode_preservation_facts,` from the re-export in
`crates/voom-plan/src/planner/audio/mod.rs:14` and from the re-export in
`crates/voom-plan/src/audio.rs:5`. (Leave the preceding `codec.is_none()` check
and the `current_container` guard in `transcode_audio_shape` intact — they are the
floor.)

> The workspace will NOT compile until Step 5 is done — `voom-control-plane` still
> imports the symbol. This is the expected mid-edit state; do not commit here.

- [ ] **Step 5: Remove the gate check and import in `voom-control-plane`**

In `crates/voom-control-plane/src/audio/selection.rs`, delete the block
(currently lines ~56-60):

```rust
    if !selected.iter().all(has_transcode_preservation_facts) {
        return Err(audio_block_error(
            AudioPlanningBlock::InsufficientSnapshotFacts,
        ));
    }
```

Remove `has_transcode_preservation_facts,` from the `use voom_plan::audio::{ … }`
import (lines ~3-7).

- [ ] **Step 6: Build the whole workspace and run both crates' tests — all pass**

Run: `cargo build --workspace --all-features 2>&1 | tail -20`
Expected: compiles with no `unused import` / `dead_code` warnings.

Run: `cargo test -p voom-plan 2>&1 | tail -20 && cargo test -p voom-control-plane --lib 2>&1 | tail -20`
Expected: both PASS, including the two new `voom-plan` tests and the rewritten
control-plane test.

Run: `rg -n has_transcode_preservation_facts crates/ ; echo "exit:$?"`
Expected: `exit:1` (no matches) — the symbol is gone from the tree.

- [ ] **Step 7: Commit (single atomic commit across both crates)**

```bash
git add crates/voom-plan/src/planner/audio/selection.rs \
        crates/voom-plan/src/planner/audio/selection_test.rs \
        crates/voom-plan/src/planner/audio/mod.rs \
        crates/voom-plan/src/audio.rs \
        crates/voom-control-plane/src/audio/selection.rs \
        crates/voom-control-plane/src/audio/selection_test.rs
git commit -m "fix(plan): drop audio-transcode preservation-fact gate

A transcode operation is built from stream references only; no per-stream
descriptive fact reaches the worker. Remove has_transcode_preservation_facts
and its planner and control-plane call sites so title-less (and language/
channels/commentary-less) streams plan. The codec + container floor in
transcode_audio_shape is the sole plannability gate. Removed atomically
across both crates so every commit builds.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Real-media proof — un-bake titles from the combined-flow fixture

The Sprint 16 combined-flow integration test baked an audio `title` into every
track purely to satisfy the now-removed gate. Removing the titles turns the test
into a real-media proof of the issue's acceptance criterion: a title-less
`remux → transcode → audio` chain commits all three phases. This test is
ffmpeg-gated; do not change its gating.

**Files:**
- Modify: `crates/voom-control-plane/tests/phase_barrier_combined_flow.rs`

- [ ] **Step 1: Remove the title metadata from the fixture generator**

In `generate_combined_fixture` delete the two title arg pairs (currently lines ~561-562 and ~565-566):

```rust
            "-metadata:s:a:0",
            "title=Main",
```
and
```rust
            "-metadata:s:a:1",
            "title=Castellano",
```

Replace the explanatory comment above them (currently lines ~553-558) with:

```rust
            // ADR-0011: the audio-transcode planner no longer requires a per-
            // stream title/commentary. These tracks are deliberately title-less
            // to prove a title-less remux -> transcode -> audio chain commits;
            // only language + disposition are set (disposition:a:N clears the
            // comment flag to a concrete false).
```

- [ ] **Step 2: Update the test's module doc comment**

The module doc comment (currently lines ~54-63) explains the audio phase commits
only because the re-probe satisfies the strict title/commentary gate. Replace the
sentence starting "This commits only because…" through the end of that paragraph
(the `has_transcode_preservation_facts` reference and the ffprobe-fix sentence)
with:

```rust
/// * The audio phase's `transcode audio to opus where lang in [eng, und]` plans
///   against the *re-probed* transcode output. Per ADR-0011 the planner gates
///   transcode plannability on the source codec + container only, so this commits
///   even though the fixture's audio tracks are title-less — the case real media
///   hits because muxers do not synthesize a title.
```

- [ ] **Step 3: Run the combined-flow test (requires ffmpeg/mkvmerge/ffprobe)**

Run: `cargo test -p voom-control-plane --test phase_barrier_combined_flow 2>&1 | tail -30`
Expected: PASS — three `Completed` phase rows, three `Committed` per-file rows,
against title-less audio. If the toolchain binaries are absent the test is
skipped/ignored by its existing gate; note that and rely on CI's integration job.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-control-plane/tests/phase_barrier_combined_flow.rs
git commit -m "test(control-plane): prove title-less audio chain commits

Drop the synthetic audio titles the combined-flow fixture baked in only to
satisfy the removed preservation gate. The remux -> transcode -> audio chain
now commits against title-less audio, the real-media case from #184.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Full guardrails

- [ ] **Step 1: Run the full CI suite locally**

Run: `just ci`
Expected: `fmt-check`, `lint` (clippy `-D warnings`), `check-test-layout`,
`test`, `doc`, `deny`, `audit` all green, zero warnings.

- [ ] **Step 2: If anything is red, fix it before proceeding**

Likely follow-ons: a `doc` build that no longer links the deleted symbol, and any
stray reference (`rg has_transcode_preservation_facts crates/` must be empty).

---

## Self-Review

**Spec coverage (ADR-0011 Decision):**
- Remove gate in `transcode_audio_shape` → Task 1 Step 4.
- Remove gate in `transcode_selection_from_payload_and_snapshot` → Task 1 Step 5.
- Delete function + both re-exports → Task 1 Step 4.
- Deterministic unit lock (Planned for fact-less, Blocked for codec-less) → Task 1 Step 1.
- Control-plane runtime lock (fact-less streams select) → Task 1 Step 2.
- Real-media proof (title-less chain commits) → Task 2.
- Closeout spec resolved note → already committed (pre-req).

**Commit-ordering / green-at-every-commit:** the cross-crate symbol removal is a
single commit (Task 1 Step 7); the workspace builds at every commit boundary
(verified by Step 6's `cargo build --workspace` before the commit). Task 2's
fixture change is independent and self-contained.

**Placeholder scan:** none — every code step shows exact code/commands.

**Type consistency:** `transcode_audio_shape`, `AudioPlanShape::{Planned,Blocked}`,
`AudioPlanningBlock::InsufficientSnapshotFacts`, `SnapshotAudioStreamFact`,
`AudioDispositionFact`, the `audio_fact`/`snapshot_with_audio_facts` (voom-plan,
`audio_fact(commentary: Option<bool>)`) and `audio(id, index, codec, language,
title, commentary)` / `snapshot_with_streams` / `transcode_payload` (control-plane)
helpers all exist with the signatures used here (verified against current sources).
