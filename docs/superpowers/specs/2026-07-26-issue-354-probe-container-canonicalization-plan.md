# Issue #354 Probe Container Canonicalization Implementation Plan

**Goal:** Make durable ffprobe container facts deterministic policy inputs so
stored reports and coordinator replanning treat supported MKV and MP4 outputs
as their published policy containers and fail closed for every other spelling
or shape.

**Design:** Canonicalize only in
`voom-control-plane::media_snapshot::planning_input`. Preserve raw durable
snapshot JSON. Use an exact, case-sensitive allowlist and project unsupported
or malformed values as an absent container fact.

**Success criteria:**

- every accepted alias maps exactly as specified by the design;
- malformed, unknown, padded, reordered, and unpublished values block planning;
- stored plan/report and coordinator report paths observe canonical values;
- real MKV outputs replan as compliant `NoOp` while retaining raw probe facts;
- the phase-barrier lineage fixture still produces V0 -> V1 -> V2 for two
  independently necessary profiles; and
- focused tests, `prek run`, and `just ci` are warning-free.

## Task 1: Pin the projection contract with failing tests

**Files:**

- Modify: `crates/voom-control-plane/src/media_snapshot_test.rs`
- Modify: `crates/voom-control-plane/src/cases/policy/plans_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/coordinator/mod_test.rs`

1. Add a table test for the seven accepted exact values:
   `mkv`, `matroska`, `matroska,webm`, `mp4`, `mov,mp4`,
   `mov,mp4,m4a,3gp,3g2,mj2`, and `ogg`.
2. Exercise both supported extraction shapes: a top-level string and a
   normalizer object with string `format_name` plus unrelated fields.
3. Add a rejection table covering absent/null/wrong-typed values, malformed
   objects, empty/padded/case-shifted strings, aliases in a different order,
   duplicates, subsets/supersets, and mixed unknown tokens.
4. Change stored plan/report coverage to persist `matroska,webm`, assert
   canonical observed `mkv`, and assert `NoOp` for `container mkv`.
5. Add stored plan/report cases for unknown and malformed container values.
   Assert both paths produce `Blocked` with the existing actionable
   insufficient-container-facts diagnostic.
6. Add a focused coordinator phase-report test proving refreshed
   `matroska,webm` facts produce `NoOp` and canonical observed `mkv`.
7. Run the focused tests and confirm they fail because raw aliases are still
   copied:

   ```sh
   cargo test -p voom-control-plane --all-features --lib media_snapshot
   cargo test -p voom-control-plane --all-features --lib stored_stream
   cargo test -p voom-control-plane --all-features --lib coordinator
   ```

## Task 2: Pin real-media replanning and independent lineage

**Files:**

- Modify: `crates/voom-control-plane/tests/remux_flow.rs`
- Modify: `crates/voom-control-plane/tests/video_transcode_flow.rs`
- Modify: `crates/voom-control-plane/tests/video_profile_flow.rs`
- Modify: `crates/voom-control-plane/tests/audio_transcode_flow.rs`
- Modify: `crates/voom-control-plane/tests/phase_barrier_flow.rs`
- Modify: `crates/voom-cli/tests/multi_phase_flow.rs`
- Modify: `crates/voom-cli/tests/multi_phase_preview_envelope.rs`
- Modify: `crates/voom-cli/tests/compliance_envelope.rs`
- Modify these files under `crates/voom-cli/tests/snapshots/`:
  - `multi_phase_preview_envelope__compliance_report_previews_combined_multi_phase_policy.snap`
  - `compliance_envelope__execute_scanned_remux_outputs_committed_file_phase.snap`
  - `compliance_envelope__execute_scanned_remux_existing_target_outputs_failure_envelope.snap`

1. Keep every raw committed-snapshot assertion at `matroska,webm`.
2. Change existing video, named-profile video, and audio authoritative
   replanning assertions from `Planned` to `NoOp`, and from raw observed
   `matroska,webm` to canonical `mkv`.
3. Add the same authoritative stored replanning assertion to the remux flow.
4. Replace the phase-barrier fixture's repeated default HEVC operation with a
   dependent `hevc-archive` phase. Assert:
   - the intermediate `default-hevc` and terminal `hevc-archive` paths;
   - phase-zero `yuv420p` and phase-one `yuv420p10le` observations;
   - both committed versions and reprobe snapshots; and
   - the existing V0 -> V1 -> V2 `produced_from` and ticket-source lineage.
5. Apply the same independently necessary `hevc-archive` second phase to the
   CLI multi-phase execution fixture. Preserve its two-commit and durable
   report-read-back assertions.
6. Before their snapshot assertions, add semantic CLI assertions that the
   multi-phase preview observes canonical `mkv` and its already-satisfied remux
   is `NoOp`, and that both scanned-remux executions observe canonical `mp4`.
   These assertions pin behavior without predicting content-derived IDs.
7. Run the four generated-media flows and confirm the new `NoOp` assertions
   fail under raw alias comparison. Run the phase-barrier test separately and
   confirm the independent lineage fixture passes. Run the affected CLI tests
   and confirm the semantic canonical-value assertions fail before snapshot
   review:

   ```sh
   cargo test -p voom-control-plane --test remux_flow
   cargo test -p voom-control-plane --test video_transcode_flow
   cargo test -p voom-control-plane --test video_profile_flow
   cargo test -p voom-control-plane --test audio_transcode_flow
   cargo test -p voom-control-plane --test phase_barrier_flow \
     phase_barrier_chains_committed_artifact_into_the_next_phase
   cargo test -p voom-cli --test multi_phase_flow
   cargo test -p voom-cli --test multi_phase_preview_envelope
   cargo test -p voom-cli --test compliance_envelope
   ```

## Task 3: Implement the single canonical projection

**Files:**

- Modify: `crates/voom-control-plane/src/media_snapshot.rs`

1. Add a private extraction-and-canonicalization function that accepts only a
   string container or an object with string `format_name`.
2. Match the extracted string against the exact design allowlist. Return the
   policy value as an owned string for recognized input and `None` otherwise.
3. Call it once from `planning_input`; do not alter snapshot persistence,
   ffprobe normalization, planner comparison, or caller-specific code.
4. Re-run every focused test from Tasks 1 and 2. All must pass.
5. Regenerate the CLI multi-phase preview golden and both scanned-remux
   compliance-execution goldens. Review canonical `mkv`/`mp4` observations,
   statuses, diagnostics, and content-derived report identities:

   ```sh
   cargo insta review
   ```

6. Mutate one accepted mapping and one rejection case locally, confirm the
   relevant table tests fail, then restore the implementation.

## Task 4: Reconcile current architecture documentation

**Files:**

- Modify: `docs/adr/0007-phase-barrier-coordinator.md`
- Modify:
  `docs/adr/0008-per-phase-report-regenerated-against-refreshed-facts.md`
- Modify:
  `docs/adr/0009-resume-opens-new-job-reconciles-prior-rows.md`

1. State that durable probe containers are canonicalized at the shared
   planning projection boundary and unknown values fail closed.
2. Remove current consequences and resume rationale that claim canonical MKV
   output necessarily replans as non-compliant.
3. Preserve the ADRs' historical decisions: ticket-backed idempotency,
   bounded replanning, refreshed per-phase reports, and recorded-phase resume
   behavior do not change.
4. Search the three ADRs for stale raw-alias claims and inspect every match:

   ```sh
   rg -n 'matroska,webm|verbatim|container.*replan|replan.*container' \
     docs/adr/0007* docs/adr/0008* docs/adr/0009*
   ```

## Task 5: Verify, review, and commit

1. Re-read the complete `origin/main...HEAD` diff for unrelated work,
   duplicate mappings, stale comments, and scope leakage into #331, #332, or
   #336.
2. Run:

   ```sh
   just fmt
   git diff --check
   prek run
   just ci
   ```

3. Commit the implementation as one logical behavior change:

   ```sh
   git add crates/voom-control-plane crates/voom-cli/tests docs/adr
   git commit -m "fix(control-plane): canonicalize probe containers"
   ```

4. Run the adversarial review loop to approval, fixing and independently
   verifying each defensible finding. Then run three simplification passes.
5. Rebase onto current `origin/main`, rerun `just ci`, push the branch, open a
   PR closing #354, wait for green CI, and merge before starting #331.
