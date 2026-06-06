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

All commands emit a single JSON envelope on stdout (`run-local` additionally
prints a `ready` line, see below); logs go to stderr.

### 1. Initialize the database (once)

```
voom init
```

`run-local` and every command below open an *existing* database via `connect`,
which never creates or migrates it (ADR-0003). Running them before `voom init`
yields a `DB_UNREACHABLE`/schema error envelope, not a crash.

### 2. Start the workers (foreground, one terminal each)

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
and hits the missing-worker path. Stop a worker with Ctrl-C (SIGINT) or SIGTERM:
it shuts the child down and retires its registration. A hard `kill -9` skips the
retire; the next `run-local --kind <same>` self-heals the stale row on startup,
and `execute` liveness-checks each endpoint before dispatch and refuses to use a
dead one (with an actionable error naming the `run-local` command to start).

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
voom policy create --slug remux-hevc --file remux-and-hevc.voom
```

Capture `version_id` from the envelope (this is the accepted version). The sample
policy (`crates/voom-control-plane/tests/fixtures/policies/remux-and-hevc.voom`):

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
- **A real-library run will partially succeed** (some files committed, some
  failed). Re-running `compliance execute` resumes via the Sprint 16 per-file-phase
  resume path (issue-165) — already-completed files are not redone. Read partial
  state with `voom compliance report --job-id <job_id>`.
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
of colliding. A single-directory run (no shared subtree) promotes flat, as
before.

## Known limitations

- **Duplicate source basenames still block a `--all` run (issue #199).** Even
  with subdir-preserving output, the phase-barrier coordinator derives each
  file's branch id from its path *stem* and rejects a stem collision across the
  active set (`active files … both derive branch id …`), so a library with two
  files that share a basename across subdirectories (e.g. `S01/episode.mkv` and
  `S02/episode.mkv`) fails fast before any work runs. Until #199 lands, run
  `--all` only against libraries with unique basenames, or scope each run to a
  subtree with no basename clashes.
- **Same-stem, different-extension siblings in one directory collide** (e.g.
  `S01/episode.mkv` and `S01/episode.mov`). Both derive the same branch id and
  the same output basename; the run fails (at the branch-id check, per #199).

## Teardown

Ctrl-C each `run-local` (it retires its worker and prints a final envelope).
`voom worker list` should then show no live local workers.
