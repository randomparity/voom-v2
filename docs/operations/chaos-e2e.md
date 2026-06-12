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

The Ubuntu runner's apt ffmpeg package may lag Chaos Librarian's minimum. The
workflow therefore installs ffmpeg/ffprobe from a pinned 7.x archive and
verifies its checksum before running the suite.

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
