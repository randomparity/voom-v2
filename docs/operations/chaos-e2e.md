# Chaos Librarian E2E

The Chaos Librarian deterministic E2E suite runs with:

```bash
just chaos-e2e-ci
```

It is intentionally outside default `just ci` because it requires `uv`, Python
3.13, ffmpeg/ffprobe 7.0+, MKVToolNix, and the pinned Chaos Librarian submodule.
Setup fails unless Chaos Librarian reports static, filesystem-mutation, and
media-mutation readiness from `chaos-librarian capabilities --json`.

Maintainers should run the manual `chaos-e2e` GitHub Actions workflow before or
after changes that affect:

- the Chaos Librarian integration;
- media scan or observed-state export behavior;
- ffprobe, ffmpeg, or artifact verification workers;
- policy report or execution paths exercised by the Chaos fixtures.

The workflow is not a required merge gate. It exists to make the heavy media
suite reproducible in a clean runner while runtime cost and tool availability
are still being characterized.

The Ubuntu runner's apt ffmpeg package lags Chaos Librarian's minimum. The workflow
therefore resolves the oldest BtbN release series meeting the 7.0+ floor at run time and
asserts the installed binary's major version against that floor; neither the version nor its
bytes are pinned. See `docs/adr/0078-runtime-resolved-ffmpeg-series.md`.

## Bumping the Chaos Librarian pin

Moving the submodule to a new revision requires updating the recorded SHA in
three places, or readiness checks fail before any test runs:

- `crates/voom-cli/tests/chaos_librarian_e2e.rs`
  (`chaos_librarian_submodule_is_pinned_and_ready`)
- `scripts/chaos-e2e-local.sh` (submodule status guard)
- `scripts/test-chaos-e2e-local.sh` (faked `git submodule status` output)

Also re-check the observed-state contract version: the exporter in
`crates/voom-cli/tests/support/observed_state.rs` emits a literal
`schema_version` that must match `schemas/observed-state.schema.json` in the
submodule, and `compare` strictly checks optional probe stream fields
(`channel_layout`, `title`, `role`) whenever the oracle records them — see
chaos-librarian issue #222 for the consumer-side normalization rules.

## Fixed-seed corpus

The ignored `fixed_seed_corpus_production_run_and_replay` case explicitly generates
`fuzz-regression` / `adversarial-media`, seed 248. Generated YAML must byte-match the
pinned upstream fixture. Coverage reads recipe parameters, not just action names:

| Family | Required recipe cells |
| --- | --- |
| Track flags | multiple defaults, no defaults, default+forced, forced-only |
| Metadata | title, year, long Unicode, control characters, deletion, missing tags, eng/fra labels |
| Structural | MP4 moov at start, MP4 moov at end, MKV Cues, MP3 after 16 frames |

The unchanged golden contains 18 assets, eleven timeline events, at most two seconds
per asset and eight audio/subtitle streams. Both the complete run and its genuine
replay get separate real owner scans and DB exports; comparisons require observed
metadata/dispositions and semantic probe facts. They do not require encoded byte
identity across tool versions. The test uses a 30-second wall-clock window; a prefix
fails the eleven-event assertion. Ordinary upstream lane selection remains unchanged.

Supplemental cases are explicitly reduced seed-248 lane slices, not full random tails:
`malformed` retains `fuzz_0005_corrupt_tags` (64 null bytes on `asset_005`) and healthy
`asset_007`; `network-fs-chaos` retains its first eight events and eight assets, including
paired unmount/remount and acquire/release. Both slices bound media to 0.2 seconds and
SD video, retain the original generated YAML as provenance and revalidate the model.
An independent healthy ASS sidecar uses the landed track fixture recipe.

The network case asserts six materialization receipts: permissions, quota, readonly,
stale-handle, remount and release. Only permissions/readonly are enforced on disk;
the others are audit-only simulations. Actual modes are checked before and after
upstream restoration, followed by a production scan of the recovered corpus. This
proves simulation application/restoration and recovered-state readability, not VOOM
handling transient ENOSPC, ESTALE, EAGAIN or unavailability, and not real NFS behavior.

Local runs write `timeline-summary.json` with applied and scheduled counts and a
`timeline_complete` boolean. A successful short prefix is explicitly reported incomplete.
The bounds and retained failure lifecycle below are owned by the consumer runner.

## Bounded local and soak runs

The three existing entrypoints now run through a POSIX process-group supervisor.
Ordinary `just chaos-e2e-local` retains its existing scenario and lane selection.
Soak explicitly generates `fuzz-regression` / `adversarial-media`, seed 248:

```bash
just chaos-e2e-soak
CHAOS_SEED=249 CHAOS_LANE=adversarial-media CHAOS_DURATION=2m \
  CHAOS_MAX_SECONDS=600 just chaos-e2e-local
```

Supply seed and lane together. Each invocation needs a new or empty `CHAOS_WORKDIR`;
otherwise a private directory below the repository’s pinned `.test-tmp/` storage root
is created and its location is printed.
Repeat with a fresh directory, recording each seed rather than using an implicit lane:

```bash
for seed in 248 249 250; do
  CHAOS_SEED="$seed" CHAOS_LANE=adversarial-media CHAOS_DURATION=2m \
    CHAOS_MAX_SECONDS=600 just chaos-e2e-local || break
done
```

| Setting | Default | Meaning |
| --- | --- | --- |
| `CHAOS_MAX_SECONDS` | local 900; heavy 2700; soak 7500 | Whole command deadline, including setup/build |
| `CHAOS_MAX_MIB` | 4096 | Aggregate logical bytes in the private run tree |
| `CHAOS_MAX_PROCESSES` | 128 | Live processes in the inherited group |
| `CHAOS_BUNDLE_MIB` | 64 | Total published diagnostic bytes, including its index and HTML |

Limits accept positive integers; maximums are respectively 86400 seconds, 1048576 MiB,
4096 processes and 1024 MiB. Seed accepts up to twenty ASCII digits; lane uses lowercase
letters, digits and hyphens. Upstream validates the actual supported seed/lane combination.
Disk/process limits are sampled every 200ms and can overshoot during concurrent writes;
they are abort thresholds, not filesystem quotas. Cargo artifacts and uv environments/cache
are outside run-data accounting. The supervisor stops remaining descendants on success,
failure, deadline and INT/TERM, escalating TERM to KILL after two seconds. Host crashes,
SIGKILL of the supervisor and deliberately escaped sessions require the host/CI owner.
Exit 124 means deadline, 125 resource/runner/cleanup failure, 130 INT and 143 TERM;
an existing command failure keeps its original exit status.

## Failure bundles and replay

`WORKDIR/bundle/runner.json` records the pin, requested selection, actual per-run seeds
and applied-event counts when available, tool versions, limits, duration, status, hashes,
missing artifacts, omissions and visualizer outcomes. An unusually large index is explicitly
marked `index_truncated`; the bundle byte cap still applies. Selected scenario/replay/manifests,
journal, asset/observed/comparison reports and synthetic process logs retain their relative
paths. A missing replay before materialization is explicitly unavailable; a timeout bundle
is not evidence of a complete timeline. Local scans do not produce the heavy suite's
observed-state/comparison reports, so those may be missing. #746 owns semantic replay checks.

Only regular, singly linked allowlisted files are copied. Links, special files, media,
databases, agent configuration, credentials and environment dumps are excluded. Files matching
known credential patterns are omitted rather than published. This boundary serves repository
synthetic fixtures; review diagnostics before sharing a custom scenario. Raw local workdirs
remain private and can contain DB/config; never upload them. Heavy raw evidence is removed after complete collection; missing replay evidence or
collection omissions retain the private raw tree for inspection. Cleanup failures also report
any remaining raw evidence. A successful heavy command
with no collected replay fails with exit 125. The weekly/manual workflow uploads only the bundle on failure, for seven
days, with separate artifact names for each rerun attempt; it introduces no required PR chaos gate or additional schedule.

Replay a retained synthetic run using its `replay.json` in a fresh bounded output directory:

```bash
export REPLAY_BUNDLE=/absolute/path/to/bundle/evidence/RUN/replay.json
CHAOS_MAX_SECONDS=600 python3 scripts/chaos-e2e-bounded.py local -- bash -c '
  cd third_party/chaos-librarian
  uv run --offline chaos-librarian replay "$REPLAY_BUNDLE" \
    --out "$CHAOS_WORKDIR/replay" --json
'
```

Use the same reviewed pin and equipped tools. Replay regenerates media; tool-version changes
can change encoded bytes. Compare semantic observations using the heavy acceptance case.
The collector renders only the validated bundle copy, so omitted inputs cannot return through
HTML. It attempts `uv run --offline --no-sync python scripts/visualize_run.py RUN
--output PATH` with a total ten-second diagnostic deadline. It needs the existing synced
upstream environment; unavailable dependencies or malformed partial evidence are reported,
and visualization cannot replace an original failure. Version probes have their own two-second
deadlines; diagnostic cleanup remains bounded after the main command deadline.

Focused lifecycle, retention and publication checks:

```bash
just chaos-e2e-bounded-selftest
just chaos-e2e-local-script-test
just acceptance-bootstrap-selftest
```

## Measured equipped cost

On Linux x86_64 with ffmpeg/ffprobe 8.0.1, MKVToolNix 97, uv 0.13.4 and four Rust
test threads, the repaired runner passed all 19 heavy cases in 110.98 seconds. Setup/build
and test execution took 115.171 seconds; version probes, collection and visualization brought
the measured total to 126.963 seconds. The bundle was 2,536,611 bytes with 33 replay runs,
including both seed-248 eleven-event runs. Offline visualization made 24 attempts within its
ten-second budget: 21 succeeded, two partial runs failed and the final attempt timed out;
the successful command result stayed unchanged. These measurements support the existing
weekly/manual schedule only; they do not authorize expanded scheduling.

An earlier cold-build run passed the same 19 cases (120.50 seconds; 201.392 seconds including
setup/build) but failed artifact acceptance because hidden Rust temp directories were excluded.
That runner defect was reproduced and fixed with a visible retained-directory prefix and a
missing-replay failure guard. The second equipped run above verified the correction; the first
run is not an overall acceptance pass. No unrelated test timeout or product source was changed.

A focused final-collector check then forced exit 17 after copying those synthetic diagnostics:
it preserved exit 17, retained 33 replay runs and a complete 1,014-file index in 2,688,780 bytes,
and recorded upstream Python 3.13.16. Replaying seed 248 from that failure bundle after raw
cleanup succeeded with eleven applied events. This checks publication/replay portability;
it adds no new semantic corpus acceptance claim.
