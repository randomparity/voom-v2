# Runbook: Operator Real-Media Execution

Run the real-media pipeline end-to-end from the `voom` CLI: scan a library, author
a policy, stand up real workers, and execute remux-to-MKV + transcode-to-HEVC.
This is the operator procedure behind the Sprint 17 slice
(`docs/superpowers/specs/2026-06-05-voom-sprint-17-slice-operator-execution-design.md`).

## Prerequisites

- `ffmpeg`, `ffprobe`, and `mkvtoolnix` (`mkvmerge`) on `PATH`. `voom worker
  run-local` fails fast with a dependency error if its tool is missing.
- One database, shared by every command below via `VOOM_DATABASE_URL`
  (e.g. `export VOOM_DATABASE_URL=sqlite:///var/lib/voom/voom.db`). Use a database
  dedicated to this library — the whole-library input builder (`--all`) selects
  every scanned video file in the DB.

## Procedure

All commands emit a single JSON envelope on stdout; logs go to stderr.
`run-local` is the documented exception — its stdout is a two-line contract
(readiness line, then the retirement envelope on shutdown). See the
[run-local stdout contract](#run-local-stdout-contract) note below.

### 1. Initialize the database (once)

```
voom init
```

`run-local` and every command below open an *existing* database via `connect`,
which never creates or migrates it (ADR-0003). Running them before `voom init`
yields a `DB_UNREACHABLE`/schema error envelope, not a crash.

### 2. Start the workers (foreground, stdin kept open)

```
# terminal A
voom worker run-local --kind ffmpeg
# terminal B
voom worker run-local --kind mkvtoolnix
```

Each registers a worker, spawns the bundled binary, records its live endpoint, and
supervises it. **Wait for each to print its readiness line** before step 4:

```
{"status":"ready","worker_id":12,"kind":"ffmpeg","endpoint":"127.0.0.1:53017"}
```

Running `compliance execute` before both workers are ready races the registration
and hits the missing-worker path. `run-local` is a foreground supervisor: it
retires the worker on Ctrl-C (SIGINT), SIGTERM, or stdin EOF. Start it in a
terminal, PTY session, or service wrapper that keeps stdin open for as long as
the worker should be live; a non-interactive launcher that closes stdin after
startup will print `ready` and then immediately retire the worker. A hard
`kill -9` skips the retire; the next `run-local --kind <same>` self-heals the
stale row on startup, and `execute` liveness-checks each endpoint before dispatch
and refuses to use a dead one (with an actionable error naming the `run-local`
command to start).

`ffprobe` and the artifact-verify worker are *not* started this way — the control
plane spawns them as managed subprocesses as needed.

### 3. Scan the library

```
voom scan --path /mnt/pool0/test-video
```

Creates file-versions + media snapshots. Non-media files (unsupported extensions)
are excluded at scan.

### 4. Author and accept the policy

```
voom policy create \
  --slug remux-to-mkv-and-transcode-to-hevc \
  --file remux-and-hevc.voom
```

Capture `version_id` from the envelope (this is the accepted version). The slug
must match the policy identity compiled from the document; for the sample policy,
that slug is `remux-to-mkv-and-transcode-to-hevc`. The sample policy
(`crates/voom-control-plane/tests/fixtures/policies/remux-and-hevc.voom`):

```
policy "remux to mkv and transcode to hevc" {
  phase remux {
    container mkv
  }
  phase transcode {
    depends_on: [remux]
    transcode video to hevc
  }
}
```

Two phases, applied as barriers across files (ADR-0007): every file is remuxed in
the remux phase, then every file is transcoded in the transcode phase, with the
transcode operating on the remuxed output. Files already compliant for a phase are
skipped.

**`policy create` is not idempotent** — `policy_documents.slug` is `UNIQUE`. On a
re-run, either `voom policy list` to find the existing document id and
`voom policy version add --document-id <id> --file <f>`, or choose a new slug.

### 5. Build a whole-library input set

```
voom policy input create-from-scan --all --slug lib1
```

Builds one input set covering every live **video** file-version; non-video /
unprobeable files are skipped (the envelope reports `included_count` /
`skipped_count`). Capture `input_set_id`.

### 6. Execute

```
voom compliance execute \
  --policy-version-id <version_id> \
  --input-set-id <input_set_id> \
  --staging-root /var/lib/voom/staging \
  --output-dir   /mnt/pool0/test-video-out
```

## Output, re-runs, and partial failure

- **Only final artifacts land in `--output-dir`.** Each phase commits to an
  internal working area under the staging root; after the run, each file's
  terminal (chain-tip) artifact is promoted into `--output-dir`. Intermediate
  remux outputs stay in the working area, not the operator's output dir.
- **Add-only.** Promotion never overwrites. Source files are never modified. If a
  destination in `--output-dir` already exists, the run fails rather than
  overwrite.
- **A real-library run can partially succeed** (some files committed, some
  failed). Re-running `compliance execute` resumes via the Sprint 16
  per-file-phase resume path (issue-165) — already-completed files are not
  redone. Read partial state with `voom compliance report --job-id <job_id>`.
  In that report, `file_phases[*].outcome` and produced artifact IDs are the
  execution results. `phases[*].report` is the compliance snapshot captured for
  that phase, and `latest_phase_index` points at the highest-ordinal phase
  snapshot. A completed file phase can therefore carry an earlier
  `noncompliant` check that explains why work was planned; use the file-phase
  outcome and produced IDs to confirm what committed.
- **Empty / all-non-video scan:** the input set is empty and `execute` is a no-op
  reporting zero planned / zero committed.
- **Scale:** the staging working area needs free disk on the order of the produced
  output set (intermediate + final per in-flight file); transcodes are
  long-running.

## Output layout

Outputs mirror the source tree. Each terminal artifact lands under
`--output-dir` at the source's path relative to the run's common source root —
a source at `<root>/S01/episode.mkv` promotes to
`--output-dir/S01/episode.…hevc.mkv` (issue #197). Sources sharing a basename
across different subdirectories therefore land at distinct destinations instead
of colliding. The phase-barrier branch IDs are also disambiguated from the
source-relative path for colliding stems (issue #199), so a whole-library run can
include files such as `S01/episode.mkv` and `S02/episode.mkv`. A
single-directory run (no shared subtree) promotes flat, as before.

## Known limitations

- Same-stem, different-extension siblings in one directory can still collide at
  output promotion if their final operation renders the same destination
  basename. Scope the run to one sibling or choose an output directory that does
  not already contain the rendered artifact name.

## Teardown

Ctrl-C each `run-local` (it retires its worker and prints a final envelope).
`voom worker list` should then show no live local workers.

## run-local stdout contract

Unlike every other `voom` command — which emits exactly one JSON envelope per
invocation — `voom worker run-local` is a long-running foreground supervisor, so
its stdout is a **two-line contract** over the worker's lifetime, in this order
and with nothing else interleaved (all logs go to stderr):

1. A **bare readiness line**, emitted once the bundled worker has bound its
   endpoint and been registered for discovery. It is not wrapped in the standard
   envelope (no `schema_version`/`command`):

   ```
   {"status":"ready","worker_id":12,"kind":"ffmpeg","endpoint":"127.0.0.1:53017"}
   ```

   Wait for this line before dispatching work; gate on `status == "ready"`.

2. The **standard retirement envelope**, emitted once on shutdown (Ctrl-C,
   SIGTERM, or stdin EOF) after the worker row is retired:

   ```
   {"schema_version":"0","command":"worker","status":"ok",
    "data":{"worker_id":12,"kind":"ffmpeg","status":"retired"},...}
   ```

   If retirement fails, line 2 is an error envelope (`status:"error"`) instead.

A consumer can therefore read stdout as: one readiness line, then exactly one
terminating envelope. This contract is enforced end-to-end by
`crates/voom-cli/tests/run_local_stdout_contract.rs` and specified in
`docs/specs/run-local-stdout-contract.md`.
