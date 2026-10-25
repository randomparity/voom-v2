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
Aggregate soak limits, retained failure bundles and upload lifecycle remain #747.
