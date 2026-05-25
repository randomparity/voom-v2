---
name: chaos-librarian-e2e-design
description: Layered end-to-end testing design using Chaos Librarian real media fixtures and VOOM policy execution.
status: draft
date: 2026-05-25
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-12-design.md
  - third_party/chaos-librarian/README.md
---

# Chaos Librarian E2E Testing

## 1. Goal

Use Chaos Librarian as a real-media fixture generator and mutator for VOOM
end-to-end testing. The suite should exercise policy definitions against actual
files, real workers, durable workflow tickets, artifact verification, committed
outputs, and CLI JSON envelopes.

The design has two layers:

- deterministic bounded tests that are eligible for CI;
- local-only wall-clock churn and soak runs for extended real-world testing.

The first implementation should prove the harness shape with a small set of
high-signal scenarios instead of trying to cover every Chaos Librarian timeline
action.

## 2. Scope

In scope:

- Keep Chaos Librarian as a git submodule at `third_party/chaos-librarian`.
- Add VOOM-owned E2E harness code that invokes Chaos Librarian through `uv run`
  from the submodule working directory and VOOM through the checked-out Rust
  binaries.
- Materialize real media with fixed-seed scenarios for deterministic CI tests.
- Run VOOM scan, policy plan, compliance execute, report, and artifact
  inspection against those generated files.
- Exercise actual bundled workers, including ffprobe, FFmpeg transcode, and
  artifact verification workers.
- Add `just` recipes that separate CI-safe E2E runs from local wall-clock churn.
- Produce a narrow Chaos-Librarian-compatible `observed-state.json` export for
  scanner/prober final-state scenarios.
- Use Chaos Librarian `compare --mode final-state` as a required assertion for
  at least the static library baseline in the first implementation.

Out of scope for the first implementation:

- A long-running daemon or filesystem watcher.
- CI execution of wall-clock churn or soak tests.
- A broad adapter for every Chaos Librarian observed-state field.
- Replacement, deletion, or backup policy semantics.
- Support for every Chaos Librarian scenario in the checked-in corpus.
- New in-process worker shortcuts. All provider behavior remains out of
  process.

## 3. Architecture

The deterministic E2E flow is:

```text
Chaos Librarian scenario
  -> uv run chaos-librarian materialize --out <run-dir>
  -> VOOM init <ephemeral sqlite>
  -> VOOM scan <run-dir>/library
  -> VOOM policy apply/plan using real policy text
  -> VOOM compliance execute
  -> scheduler leases durable tickets to bundled workers
  -> workers probe/transcode/verify real files
  -> VOOM report/artifact inspection
  -> observed-state export for compare-enabled scenarios
  -> uv run chaos-librarian compare --mode final-state for compare-enabled scenarios
```

For the first implementation, "compare-enabled scenarios" means the static
library baseline. Other deterministic cases use direct VOOM assertions until
the observed-state exporter can represent their required oracle fields without
synthesizing unobserved facts.

The local wall-clock flow is:

```text
Chaos Librarian run --duration <duration> --speed <speed>
  + repeated VOOM scan/plan/execute/report checkpoints
  + operator-readable run summary and persisted logs
```

The E2E harness owns temporary directories, database URLs, scenario selection,
policy file paths, binary paths, environment setup, and output capture. It must
not rely on a developer's persistent VOOM database or media directory. Every
test starts from a fresh run directory and SQLite database.

The harness must initialize and validate the submodule before running:

```text
git submodule status third_party/chaos-librarian
cd third_party/chaos-librarian
uv sync --locked
uv run chaos-librarian capabilities --json
```

The reported Chaos Librarian revision, Python version, ffmpeg/ffprobe versions,
and MKVToolNix availability become part of the E2E run summary. If the
submodule is missing, dirty, on an unexpected revision, or cannot satisfy its
locked Python environment, the harness fails before creating VOOM state.

## 4. Test Layers

### 4.1 CI-Safe Deterministic Tests

Add a bounded command:

```text
just chaos-e2e-ci
```

This command runs a small Rust integration suite or shell wrapper that:

- validates Chaos Librarian capabilities before media materialization;
- fails loudly if required tools are missing;
- creates temporary fixture directories;
- runs only fixed-seed, finite scenarios;
- avoids sleeps except bounded process waits;
- emits one concise summary of generated media, VOOM operations, and failures.

Initial CI-eligible cases:

1. Static library baseline:
   - scenario: `static-library.yaml`;
   - assertion: scan/probe persists file identity and media snapshots for all
     supported files;
   - assertion: VOOM exports `observed-state.json` with relative paths, sizes,
     hashes, and probed facts, and `chaos-librarian compare --mode final-state`
     exits `0`.

2. Policy transcode required:
   - scenario: a VOOM-owned compact scenario with one H.264 video asset;
   - policy: `transcode video to hevc {}`;
   - assertion: plan includes a `transcode_video` node, execute runs the real
     FFmpeg worker, the staged artifact is verified, and the committed result is
     HEVC-in-MKV.

3. Policy transcode no-op:
   - scenario: one already-compliant HEVC MKV asset;
   - policy: `transcode video to hevc {}`;
   - assertion: plan/report mark the input compliant and no worker mutation is
     attempted.

4. Step mutation rescan:
   - scenario: `reencode-video.yaml` or `remux-container.yaml`;
   - flow: materialize, scan, apply one `chaos-librarian step`, rescan;
   - assertion: VOOM observes new file facts and policy planning responds to
     the changed codec/container facts.

5. Malformed media:
   - scenario: `malformed-container-header.yaml`;
   - assertion: scan/probe and downstream policy planning fail loudly or block
     with stable diagnostics rather than creating execution tickets for unknown
     media.

### 4.2 Local Wall-Clock Churn

Add a local-only command:

```text
just chaos-e2e-local
```

This command runs wall-clock Chaos Librarian scenarios for short local
validation. It is not part of `just ci`. It uses explicit environment variables
with bounded defaults:

```text
CHAOS_DURATION=10m        # default for local
CHAOS_SPEED=5x            # default for local
CHAOS_SCENARIO=active-library-churn.yaml
CHAOS_CHECKPOINT_INTERVAL=30s
CHAOS_EXECUTE_POLICY=0
```

The local run starts `chaos-librarian run` under a managed child process, then
performs repeated VOOM checkpoints until the configured duration ends. The
harness stops checkpoints when Chaos Librarian exits successfully, fails the run
if Chaos Librarian exits early with an error, and terminates the child process
on harness interruption while preserving the run directory. Each checkpoint
records:

- monotonic checkpoint number and wall-clock timestamp;
- Chaos Librarian run directory and latest journal offset when available;
- scan result envelope;
- policy plan/report envelope;
- whether policy execution was enabled for that checkpoint;
- worker/artifact/report envelopes for checkpoints that execute policy.

`CHAOS_EXECUTE_POLICY=0` is the default for broad churn because VOOM does not
yet define replacement/delete semantics for every mutating scenario.
`CHAOS_EXECUTE_POLICY=1` is allowed only for scenarios whose expected mutations
are covered by existing VOOM policy operations. The harness must reject
execution-enabled local runs for unsupported policies instead of silently
skipping worker mutations.

Each local scenario declares an allowlist of stable VOOM error codes that are
expected during specific timeline windows. The malformed-media scenario allows
only probe or planning diagnostics after its corruption event. The slow-copy
scenario allows only partial-file diagnostics while the slow copy is active. Any
non-allowlisted VOOM command failure fails the local run. Allowlisted transient
diagnostics are counted and reported with their stable error codes, checkpoint
numbers, and associated Chaos Librarian timeline event when available.

The harness records the exact scenario, seed, duration, speed, VOOM version,
worker versions, database path, output directory, and every checkpoint outcome.

Add a longer soak command:

```text
just chaos-e2e-soak
```

The soak recipe is explicitly opt-in and intended for extended local runs. It
must default to preserving run artifacts for inspection and must not delete
temporary output automatically unless the operator passes an explicit cleanup
flag.

Initial local-only scenarios:

- `active-library-churn.yaml` for mixed moves, deletes, restores, sidecars, and
  media changes.
- `slow-copy-materialize.yaml` or `slow-copy.yaml` for partial-file and
  checkpoint behavior.
- `move-between-roots.yaml` for identity and path reconciliation across library
  roots.
- `malformed-container-header.yaml` for repeated failure handling during churn.

## 5. Policies

The first E2E policies should be real VOOM policy text, checked into VOOM test
fixtures rather than embedded in command strings:

- `video-transcode-hevc.voom`: the Sprint 12 real mutation policy.
- `scan-only-baseline.voom`: a no-mutation policy used only when a VOOM command
  requires policy text to produce a report for a scan/probe-only scenario.
- Future policies for sidecars, subtitle extraction, remuxing, and audio
  transcode only after those operations exist in VOOM.

The harness should avoid policy-specific shortcuts. A test passes only if the
policy compiles, plans against scanned inputs, creates the intended durable
workflow records, and reports the intended outcome through public CLI or
control-plane APIs.

## 6. Observed-State Export

Chaos Librarian comparison requires a consumer-neutral `observed-state.json`.
The first VOOM adapter should be intentionally narrow:

- export current file paths under the generated library root;
- export content hashes and sizes for scanned current files;
- export container and stream facts that VOOM has durably probed;
- export lifecycle/history fields only after VOOM has equivalent durable
  evidence.

The deterministic suite can start with direct VOOM assertions and enable
`chaos-librarian compare --mode final-state` only for scenarios whose required
observed-state fields are implemented. The static library baseline must be in
that set before the first implementation is considered complete. The adapter
must not synthesize facts that VOOM did not observe.

The exporter must follow the Chaos Librarian observed-state contract:

- `run_id` comes from the generated fixture metadata, not from a VOOM job ID.
- `current_path` values are POSIX paths relative to `<run-dir>/library`.
- Absolute paths, `..`, `.`, empty path segments, and platform-specific
  separators are rejected before compare.
- `observed_ref` values are stable within one export and derived from VOOM's
  durable file identity rather than transient scan order.
- `content_hash`, `size_bytes`, and media facts are included only when VOOM has
  persisted them for the current file version.

## 7. Error Handling

The harness fails before running tests when required tools are missing:

- `uv`;
- Python 3.13 for Chaos Librarian;
- FFmpeg and ffprobe versions accepted by Chaos Librarian and VOOM workers;
- MKVToolNix tools for scenarios that require them;
- the VOOM Rust binaries under test.
- an initialized `third_party/chaos-librarian` submodule at the pinned
  repository revision.

Fixture and VOOM failures should preserve enough context to debug the run:

- scenario path and seed;
- run directory;
- SQLite database path;
- relevant VOOM JSON envelopes;
- worker stderr/log files when available;
- Chaos Librarian reports and journal files.

CI runs may delete temporary directories only after success. Failed runs should
print the preserved path when the test framework supports it.

## 8. Verification

Success criteria:

- `git submodule status third_party/chaos-librarian` reports the pinned
  revision and no dirty marker.
- `just chaos-e2e-ci` runs deterministic scenarios without using persistent
  local state.
- The static library baseline exports valid `observed-state.json` and passes
  `chaos-librarian compare --mode final-state`.
- The transcode-required case proves real policy execution through actual
  FFmpeg and artifact verification workers.
- The no-op case proves compliant media does not schedule mutation work.
- Malformed or insufficient media facts do not produce unsafe worker tickets.
- Local-only churn recipes are available, documented as non-CI, and preserve
  useful artifacts for long runs.
- Execution-enabled local churn rejects unsupported policy/scenario
  combinations before starting the run.
- Local churn reports every allowlisted transient diagnostic by stable error
  code and fails on any non-allowlisted VOOM command failure.
- Existing `just ci` remains the default CI suite unless the project explicitly
  opts into adding `chaos-e2e-ci` later.

## 9. Open Implementation Notes

- Prefer Rust integration tests for deterministic assertions that need direct
  control-plane inspection.
- Prefer shell/`just` orchestration for wall-clock local runs because these are
  operator workflows, not unit-level regressions.
- Keep Chaos Librarian scenarios that are specific to VOOM under a VOOM-owned
  fixture directory. Referencing upstream scenarios is fine for broad coverage,
  but core regression tests should not depend on upstream fixture semantics
  changing unexpectedly.
- The harness should use structured JSON parsing for VOOM envelopes and Chaos
  Librarian reports. Do not assert by grepping free-form logs.
