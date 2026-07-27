# Issue #338 — Published Grammar CLI Corpus Execution

Date: 2026-07-27
Status: Draft
Base: `main` at `e3eacd93848bca40bb68e1383b83ee96de3f8326`

## Context

The repository has four canonical policies and a row-complete published
V1/V1.1 grammar matrix:

- core mutation (`C1`);
- track selection and ordering (`T1a`–`T1c`);
- audio transform, synthesis, and extraction (`A1`);
- per-file conditions, rules, gates, and continue-on-error (`F1a`–`F1c`).

Compilation goldens and focused planner/coordinator tests already own semantic
details. The only real CLI process-boundary acceptance runs one MP4 through a
two-operation remux/video-transcode policy. It does not prove that the
canonical corpus is executable by the shipped CLI and real workers.

The campaign requires all four policies to pass through scan, compile, preview
plan, execute, commit, inspect, and verification using deterministic generated
media. The test must not add source syntax, alter compiled policy JSON, use an
in-process provider shortcut, or execute exhaustive test policies on a
production library.

## Goals

1. Execute each canonical corpus policy through the shipped `voom` binary.
2. Use the real `worker run-local` supervisors and bundled worker processes.
3. Generate the documented C1, T1, A1, and F1 media facts using host media
   tools already installed by CI.
4. Assert behavior at the CLI envelope, durable run-report, artifact evidence,
   and output-media boundaries.
5. Keep the test deterministic, diagnosable, and bounded on Ubuntu and macOS.

## Non-goals

- Production canary or full-library execution; #339 owns that operator work.
- New DSL productions, aliases, policy text, or compiled-wire changes.
- New production worker, scheduler, artifact, or coordinator behavior unless
  execution reveals a campaign-owned defect.
- A new public resume CLI. Repeated-resume equivalence is not a grammar
  production and remains owned by #330's focused coordinator tests. The F1
  process test proves fresh-run `completed` and `modified` decisions plus
  visible missing/failed predecessor behavior.
- Performance work owned by #367–#369.

## Design

### One corpus test, isolated scenario state

Add one CLI integration test target whose top-level test runs four scenario
functions serially. Each scenario owns a new temporary root and SQLite
database. Isolation prevents policy slugs, worker rows, output filenames, and
deliberate failures from contaminating another policy.

Serial execution avoids four simultaneous ffmpeg/mkvtoolnix topologies on CI.
Each scenario remains independently named in panic context and subprocess
diagnostics.

### Shared real-process harness

Move the reusable mechanics from the existing operator E2E into a
`tests/support` module:

- invoke the shipped `voom` binary with one database URL;
- parse exactly one JSON envelope and preserve stdout/stderr on failure;
- supervise `voom worker run-local` children;
- wait for readiness and verify worker identity/kind;
- close stdin, require the retirement envelope, and reap the child;
- kill and reap a child from `Drop` after a test panic;
- list output paths deterministically.

The existing operator test adopts the shared harness. This prevents two
slightly different process-lifecycle implementations from becoming test
contracts.

The corpus test prebuilds ffmpeg, mkvtoolnix, ffprobe, and verifier worker
binaries before spawning any process and hides stale fake-ffprobe siblings.
Every scenario starts the ffmpeg and mkvtoolnix supervisors because corpus tool
preflight and later phase planning must observe the real registered providers.

All synchronous subprocesses, including `voom`, fixture generators, media
inspectors, and prebuild commands, use one timeout-owning runner. It drains
stdout and stderr concurrently, polls for completion until a fixed deadline,
then kills and reaps the child before reporting its arguments and captured
output. Supervisor shutdown has its own deadline and the same kill-and-reap
fallback. No test process can wait forever on a child or a full output pipe.

### Shipped CLI flow

Every scenario performs only shipped commands for VOOM state transitions:

1. `voom init`;
2. `voom scan --path <scenario-library>`;
3. `voom policy create --slug <scenario> --file <canonical-source>`;
4. `voom policy input create-from-scan --all --slug <scenario-input>`;
5. `voom compliance report --policy-version-id ... --input-set-id ...`;
6. `voom compliance execute ...`;
7. `voom compliance report --job-id ...`;
8. relevant `voom artifact`, `voom event`, `voom ticket`, and `voom job`
   inspection commands.

External `ffmpeg`, `ffprobe`, and `mkvmerge -J` commands generate or inspect
media bytes; they never mutate VOOM durable state.

Each command assertion checks exit status, envelope status, command name, and
the exact allowed warning set. Untagged-track warnings in T1/A1 are asserted
when the generated fixture requires them; all other warnings fail the test.
Expected F1 partial failure checks the error envelope and its partial `data`,
then reads the same durable job through shipped inspection commands.

### Generated media

Fixture creation is explicit and local to test support. Commands run with
overwrite enabled only inside the new temporary directory. Intermediate
generation inputs live in a scratch directory outside the scanned library so
only the documented media enters the policy input set.

#### C1

Create one two-second MP4 with:

- 1920×1080 H.264/yuv420p video;
- one English stereo AAC track;
- the audio track marked default;
- non-zero duration and bitrate facts.

The source is intentionally non-Matroska and non-HEVC so container and video
phases both commit.

#### T1a–T1c

Generate small H.264 video and reusable audio/subtitle inputs, then assemble
three MKVs with mkvmerge:

- widths 1920, 1024, and 512 with proportional documented heights;
- default English 5.1 E-AC-3, English stereo AAC, English commentary AAC, and
  untagged stereo AAC;
- forced English, untagged, and titled English `Signs` subtitles;
- one font attachment and one non-font attachment;
- the documented initial track order and exact dispositions.

The font attachment uses a small deterministic test payload with a font MIME
type; the non-font attachment uses a different payload and MIME type. The test
does not claim either payload is a valid font parser input.

#### A1

Assemble one MKV containing H.264 video plus default English 5.1 E-AC-3,
English stereo AAC, untagged stereo AAC, and Japanese commentary AAC.
Distinct generated tones let the test identify source streams through decoded
audio fingerprints without pinning encoder-version-specific bytes.

#### F1a–F1c

Generate the three documented files:

- `modify.mp4`: H.264 MP4, two audio and two subtitle tracks;
- `already-normalized.mkv`: HEVC Matroska with equivalent stream cardinality;
- `fail.mp4`: the same planning facts as `modify.mp4`.

After scan, derive F1c's source file-version id from the scan envelope. Before
execution, create the exact remux working target
`<staging>/.committed/remux/v<file-version-id>/fail.remux.mkv` with different
bytes. This makes F1c fail at the add-only operation commit guard before any
dependent phase dispatch, while F1a continues and F1b exercises
completed-but-unmodified gates. A flat `fail.mkv` in the final output directory
would not exercise this guard because terminal promotion happens only after
the phase chain completes.

Fixture-generation helpers assert the generated facts before VOOM scans them,
including F1's bitrate condition being strictly above 1,000,000 bits per
second. This separates a bad fixture from an execution regression.

### Scenario oracles

#### C1 oracle

- scan ingests one file with no unexpected skip/failure;
- preview contains the expected remux, named-profile video transcode, and
  verification nodes;
- execute and stored report show committed container and encode phases and a
  verified phase;
- artifact verification evidence identifies the unchanged verified output;
- ffprobe reports Matroska and HEVC.

#### T1 oracle

- all three generated variants enter one input set;
- the ordered phases commit where their documented predicates match;
- every successful file has a verified final phase;
- `mkvmerge -J` proves exact surviving track kind order, language, title,
  commentary/forced/default dispositions, and attachment MIME types;
- T1a, T1b, and T1c prove the distinct `best`, `none`/`preserve`, and
  remove-all-subtitle alternatives.

#### A1 oracle

- audio phases report all three transcode targets and all three synthesis
  targets;
- filtered extraction yields only the commentary AAC source;
- bare extraction yields one output for every selected audio stream without
  target collision;
- execute and stored report expose matching ordered extraction output ids,
  unique result file-version/location/snapshot/artifact identities, exact
  source snapshot stream ids/provider indexes, and synthesized companion
  lineage;
- every sidecar exists, ffprobe proves codec/channel facts, and within-run
  decoded tone fingerprints distinguish source identity where the selections
  differ;
- filtered commentary extraction and bare commentary extraction may contain
  identical bytes; equality does not weaken their distinct durable output
  identities or exact source lineage;
- the final file phase is verified with durable evidence.

#### F1 oracle

- execute exits with the expected policy-execution failure envelope and partial
  data, not an unstructured process failure;
- F1a commits inspect and normalize, enters organize through `modified`, and
  records verification;
- F1b records inspect/normalize completion without mutation and does not enter
  organize because `modified` is false;
- F1c records the deterministic existing-target failure, dispatches no
  dependent mutation, and produces no success verification;
- the job, tickets, file-phase summaries, and events exposed by shipped CLI
  inspection agree with the partial envelope;
- the pre-existing `fail.mkv` bytes remain unchanged.

### Output discovery

Tests derive produced paths and artifact IDs from CLI envelopes and stored run
reports. They do not assume directory counts when an operation legitimately
emits multiple files. Filesystem enumeration is used only to detect unexpected
extra output and to pass observed media paths to ffprobe/mkvmerge.

### Failure and cleanup behavior

- Any command failure prints its args, exit code, stdout, and stderr.
- Any malformed envelope prints the complete stdout.
- Media-tool failures print the tool status and captured stderr.
- Every subprocess has a fixed deadline, concurrent output drains, and a
  kill-and-reap timeout path.
- Supervisor readiness and shutdown have fixed deadlines, including bounded
  retirement-envelope collection.
- RAII cleanup reaps all children after assertion panics.
- Each temporary scenario root is deleted only after children stop.
- The deliberate F1 failure is asserted through durable job, ticket, artifact,
  and event state, not only exit status or worker requests.

## Compatibility and rollback

The intended change is test-only. Canonical policy source and compiled goldens
are inputs and must remain byte-for-byte unchanged. No schema, durable payload,
CLI envelope, error-code, or worker-protocol change is planned.

The coverage matrix is corrected to distinguish two acceptance layers without
changing policy text: #338 owns the fresh real-CLI F1 execution witness; #330
continues to own repeated-resume equivalence through its focused coordinator
tests. Adding a public resume command solely for this test would create an
unrequested interface and is outside this issue.

If execution exposes a production defect, that change requires its own design
decision within #338 only when it is necessary to execute the already-published
corpus. Independent gaps become native #325 sub-issues with
`status:needs-triage`.

## Verification

- Deliberately break each scenario's core oracle and confirm its focused test
  fails.
- Run the corpus test from a clean target after prebuilding real workers.
- Run the existing operator E2E to prove the shared harness preserves behavior.
- Run strict affected-crate Clippy and repository structural guards.
- Keep every new helper within the repository's 100-line function limit.
- Run `just ci`.
- Require hosted cargo audit, coverage/SonarCloud, Ubuntu, and macOS checks.
